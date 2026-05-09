use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    sync::{Arc, Mutex},
};

use axum::{
    Json, Router,
    body::Bytes,
    extract::{ConnectInfo, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::post,
};
use hmac::{Hmac, Mac};
use ipnet::IpNet;
use polyphony_core::{DispatchApprovalState, Issue};
use polyphony_orchestrator::{RuntimeCommand, WebhookDispatchRequest};
use polyphony_workflow::{
    WebhookTriggerConfig, WebhooksConfig, render_issue_template_with_strings,
};
use serde::Serialize;
use sha2::Sha256;
use subtle::ConstantTimeEq;
use tokio::sync::{RwLock, mpsc};
use uuid::Uuid;

type HmacSha256 = Hmac<Sha256>;

pub(crate) type SharedWebhooksConfig = Arc<RwLock<WebhooksConfig>>;

trait WebhookAuthStrategy: Send + Sync {
    fn verify(&self, headers: &HeaderMap, query: &HashMap<String, String>, body: &[u8]) -> bool;
}

struct HmacSha256Strategy {
    secret: String,
}

impl WebhookAuthStrategy for HmacSha256Strategy {
    fn verify(&self, headers: &HeaderMap, _query: &HashMap<String, String>, body: &[u8]) -> bool {
        let Some(sig_header) = headers.get("x-hub-signature-256") else {
            return false;
        };
        let sig_str = sig_header.to_str().unwrap_or_default();
        let Some(hex_sig) = sig_str.strip_prefix("sha256=") else {
            return false;
        };
        let Ok(expected_bytes) = hex::decode(hex_sig) else {
            return false;
        };
        let Ok(mut mac) = HmacSha256::new_from_slice(self.secret.as_bytes()) else {
            return false;
        };
        mac.update(body);
        let computed = mac.finalize().into_bytes();
        bool::from(computed.as_slice().ct_eq(&expected_bytes))
    }
}

struct HmacSha256HeaderStrategy {
    secret: String,
    header_name: String,
}

impl WebhookAuthStrategy for HmacSha256HeaderStrategy {
    fn verify(&self, headers: &HeaderMap, _query: &HashMap<String, String>, body: &[u8]) -> bool {
        let Some(sig_header) = headers.get(self.header_name.as_str()) else {
            return false;
        };
        let hex_sig = sig_header.to_str().unwrap_or_default();
        let Ok(expected_bytes) = hex::decode(hex_sig) else {
            return false;
        };
        let Ok(mut mac) = HmacSha256::new_from_slice(self.secret.as_bytes()) else {
            return false;
        };
        mac.update(body);
        let computed = mac.finalize().into_bytes();
        bool::from(computed.as_slice().ct_eq(&expected_bytes))
    }
}

struct TokenHeaderStrategy {
    secret: String,
    header_name: String,
}

impl WebhookAuthStrategy for TokenHeaderStrategy {
    fn verify(&self, headers: &HeaderMap, _query: &HashMap<String, String>, _body: &[u8]) -> bool {
        let Some(value) = headers.get(self.header_name.as_str()) else {
            return false;
        };
        let token = value.to_str().unwrap_or_default();
        bool::from(token.as_bytes().ct_eq(self.secret.as_bytes()))
    }
}

struct BearerStrategy {
    token: String,
}

impl WebhookAuthStrategy for BearerStrategy {
    fn verify(&self, headers: &HeaderMap, _query: &HashMap<String, String>, _body: &[u8]) -> bool {
        let Some(auth) = headers.get("authorization") else {
            return false;
        };
        let value = auth.to_str().unwrap_or_default();
        let token = value.strip_prefix("Bearer ").unwrap_or(value);
        bool::from(token.as_bytes().ct_eq(self.token.as_bytes()))
    }
}

struct QueryTokenStrategy {
    token: String,
    query_name: String,
}

impl WebhookAuthStrategy for QueryTokenStrategy {
    fn verify(&self, _headers: &HeaderMap, query: &HashMap<String, String>, _body: &[u8]) -> bool {
        let Some(value) = query.get(&self.query_name) else {
            return false;
        };
        bool::from(value.as_bytes().ct_eq(self.token.as_bytes()))
    }
}

struct NoAuthStrategy;

impl WebhookAuthStrategy for NoAuthStrategy {
    fn verify(&self, _headers: &HeaderMap, _query: &HashMap<String, String>, _body: &[u8]) -> bool {
        true
    }
}

fn build_strategy(
    auth: &str,
    secret: &str,
    header: Option<&str>,
    query: Option<&str>,
) -> Result<Box<dyn WebhookAuthStrategy>, String> {
    match auth {
        "hmac_sha256" => Ok(Box::new(HmacSha256Strategy {
            secret: secret.to_string(),
        })),
        "hmac_sha256_header" => Ok(Box::new(HmacSha256HeaderStrategy {
            secret: secret.to_string(),
            header_name: header.unwrap_or("X-Signature").to_string(),
        })),
        "token_header" => Ok(Box::new(TokenHeaderStrategy {
            secret: secret.to_string(),
            header_name: header.unwrap_or("X-Webhook-Token").to_string(),
        })),
        "bearer" => Ok(Box::new(BearerStrategy {
            token: secret.to_string(),
        })),
        "query_token" => Ok(Box::new(QueryTokenStrategy {
            token: secret.to_string(),
            query_name: query.unwrap_or("token").to_string(),
        })),
        "none" => Ok(Box::new(NoAuthStrategy)),
        other => Err(format!("unknown webhook auth strategy: {other}")),
    }
}

#[derive(Clone)]
pub(crate) struct WebhookState {
    command_tx: mpsc::UnboundedSender<RuntimeCommand>,
    config: SharedWebhooksConfig,
    delivery_counts: Arc<Mutex<HashMap<String, u64>>>,
}

impl WebhookState {
    fn next_delivery_index(&self, trigger_id: &str) -> u64 {
        let mut counts = self
            .delivery_counts
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let next = counts.get(trigger_id).copied().unwrap_or(0) + 1;
        counts.insert(trigger_id.to_string(), next);
        next
    }
}

#[derive(Serialize)]
struct WebhookResponse {
    accepted: bool,
    kind: String,
    name: String,
    delivery_index: Option<u64>,
}

#[derive(Serialize)]
struct WebhookErrorResponse {
    error: String,
}

enum WebhookAuthorizationError {
    Unauthorized(String),
    InvalidConfig(String),
}

fn authorize_request(
    auth: &str,
    secret: &str,
    header: Option<&str>,
    query_name: Option<&str>,
    source_allowlist: &[String],
    remote_addr: IpAddr,
    headers: &HeaderMap,
    query: &HashMap<String, String>,
    body: &[u8],
) -> Result<(), WebhookAuthorizationError> {
    if !source_allowlist.is_empty() && !source_allowlist_matches(source_allowlist, remote_addr) {
        return Err(WebhookAuthorizationError::Unauthorized(
            "webhook source address is not allowlisted".into(),
        ));
    }

    let strategy = build_strategy(auth, secret, header, query_name)
        .map_err(WebhookAuthorizationError::InvalidConfig)?;
    if !strategy.verify(headers, query, body) {
        return Err(WebhookAuthorizationError::Unauthorized(
            "webhook authentication failed".into(),
        ));
    }
    Ok(())
}

fn source_allowlist_matches(source_allowlist: &[String], remote_addr: IpAddr) -> bool {
    source_allowlist.iter().any(|entry| {
        entry
            .parse::<IpNet>()
            .map(|network| network.contains(&remote_addr))
            .or_else(|_| entry.parse::<IpAddr>().map(|ip| ip == remote_addr))
            .unwrap_or(false)
    })
}

async fn handle_refresh_webhook(
    State(state): State<WebhookState>,
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
    Path(provider_name): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    let config = state.config.read().await.clone();
    if !config.enabled {
        return (
            StatusCode::NOT_FOUND,
            Json(WebhookErrorResponse {
                error: "webhooks are disabled".into(),
            }),
        )
            .into_response();
    }

    let Some(provider) = config.providers.get(&provider_name).cloned() else {
        return (
            StatusCode::NOT_FOUND,
            Json(WebhookErrorResponse {
                error: format!("unknown webhook provider: {provider_name}"),
            }),
        )
            .into_response();
    };

    match authorize_request(
        &provider.auth,
        &provider.secret,
        provider.header.as_deref(),
        provider.query.as_deref(),
        &provider.source_allowlist,
        remote_addr.ip(),
        &headers,
        &query,
        &body,
    ) {
        Ok(()) => {},
        Err(WebhookAuthorizationError::Unauthorized(error)) => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(WebhookErrorResponse { error }),
            )
                .into_response();
        },
        Err(WebhookAuthorizationError::InvalidConfig(error)) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(WebhookErrorResponse { error }),
            )
                .into_response();
        },
    }

    tracing::info!(
        provider = %provider_name,
        remote_addr = %remote_addr,
        body_len = body.len(),
        "refresh webhook received, triggering tracker refresh"
    );
    let _ = state.command_tx.send(RuntimeCommand::Refresh);

    (
        StatusCode::OK,
        Json(WebhookResponse {
            accepted: true,
            kind: "provider".into(),
            name: provider_name,
            delivery_index: None,
        }),
    )
        .into_response()
}

async fn handle_trigger_webhook(
    State(state): State<WebhookState>,
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
    Path(trigger_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    let config = state.config.read().await.clone();
    if !config.enabled {
        return (
            StatusCode::NOT_FOUND,
            Json(WebhookErrorResponse {
                error: "webhooks are disabled".into(),
            }),
        )
            .into_response();
    }

    let Some(trigger) = config.triggers.get(&trigger_id).cloned() else {
        return (
            StatusCode::NOT_FOUND,
            Json(WebhookErrorResponse {
                error: format!("unknown webhook trigger: {trigger_id}"),
            }),
        )
            .into_response();
    };
    if !trigger.enabled {
        return (
            StatusCode::NOT_FOUND,
            Json(WebhookErrorResponse {
                error: format!("webhook trigger {trigger_id} is disabled"),
            }),
        )
            .into_response();
    }

    match authorize_request(
        &trigger.auth,
        &trigger.secret,
        trigger.header.as_deref(),
        trigger.query.as_deref(),
        &trigger.source_allowlist,
        remote_addr.ip(),
        &headers,
        &query,
        &body,
    ) {
        Ok(()) => {},
        Err(WebhookAuthorizationError::Unauthorized(error)) => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(WebhookErrorResponse { error }),
            )
                .into_response();
        },
        Err(WebhookAuthorizationError::InvalidConfig(error)) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(WebhookErrorResponse { error }),
            )
                .into_response();
        },
    }

    let delivery_index = state.next_delivery_index(&trigger_id);
    let dispatch = match build_trigger_dispatch(
        &trigger_id,
        delivery_index,
        &trigger,
        remote_addr,
        &headers,
        &query,
        &body,
    ) {
        Ok(dispatch) => dispatch,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(WebhookErrorResponse { error }),
            )
                .into_response();
        },
    };

    tracing::info!(
        trigger_id = %trigger_id,
        remote_addr = %remote_addr,
        issue_identifier = %dispatch.issue.identifier,
        agent = %dispatch.agent_name,
        "trigger webhook accepted"
    );
    let _ = state
        .command_tx
        .send(RuntimeCommand::DispatchWebhook(Box::new(dispatch)));

    (
        StatusCode::OK,
        Json(WebhookResponse {
            accepted: true,
            kind: "trigger".into(),
            name: trigger_id,
            delivery_index: Some(delivery_index),
        }),
    )
        .into_response()
}

fn build_trigger_dispatch(
    trigger_id: &str,
    delivery_index: u64,
    trigger: &WebhookTriggerConfig,
    remote_addr: SocketAddr,
    headers: &HeaderMap,
    query: &HashMap<String, String>,
    body: &[u8],
) -> Result<WebhookDispatchRequest, String> {
    let body_text = String::from_utf8_lossy(body).into_owned();
    let body_json = serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|value| serde_json::to_string_pretty(&value).ok())
        .unwrap_or_else(|| body_text.clone());
    let headers_json = header_map_to_json(headers);
    let query_json = serde_json::to_string_pretty(query).unwrap_or_else(|_| "{}".to_string());
    let issue_id = format!("webhook:{trigger_id}:{delivery_index}:{}", Uuid::new_v4());
    let issue_identifier = format!(
        "WEBHOOK-{}-{}",
        sanitize_identifier_component(trigger_id),
        delivery_index
    );
    let placeholder_title = format!("Webhook {trigger_id} #{delivery_index}");
    let base_issue = Issue {
        id: issue_id,
        identifier: issue_identifier,
        title: placeholder_title.clone(),
        description: Some(body_json.clone()),
        priority: None,
        state: "webhook".into(),
        branch_name: None,
        url: None,
        author: None,
        labels: vec![format!("webhook:{trigger_id}")],
        comments: Vec::new(),
        blocked_by: Vec::new(),
        approval_state: DispatchApprovalState::Approved,
        parent_id: None,
        created_at: None,
        updated_at: None,
    };

    let extra = webhook_template_vars(
        trigger_id,
        delivery_index,
        remote_addr,
        &body_text,
        &body_json,
        &headers_json,
        &query_json,
    );
    let title = trigger
        .title
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .map(|template| render_issue_template_with_strings(template, &base_issue, None, &extra))
        .transpose()
        .map_err(|error| format!("rendering webhook title failed: {error}"))?
        .unwrap_or(placeholder_title);

    let mut issue = base_issue;
    issue.title = title;
    let prompt = render_issue_template_with_strings(&trigger.prompt, &issue, None, &extra)
        .map_err(|error| format!("rendering webhook prompt failed: {error}"))?;

    Ok(WebhookDispatchRequest {
        trigger_id: trigger_id.to_string(),
        repo_id: trigger.repo_id.clone(),
        issue,
        agent_name: trigger.agent.clone(),
        model: trigger.model.clone(),
        prompt,
    })
}

fn webhook_template_vars(
    trigger_id: &str,
    delivery_index: u64,
    remote_addr: SocketAddr,
    body: &str,
    body_json: &str,
    headers_json: &str,
    query_json: &str,
) -> Vec<(&'static str, String)> {
    vec![
        ("trigger_id", trigger_id.to_string()),
        ("delivery_index", delivery_index.to_string()),
        ("remote_addr", remote_addr.ip().to_string()),
        ("body", body.to_string()),
        ("body_json", body_json.to_string()),
        ("headers_json", headers_json.to_string()),
        ("query_json", query_json.to_string()),
    ]
}

fn header_map_to_json(headers: &HeaderMap) -> String {
    let values = headers
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_string(),
                value.to_str().unwrap_or_default().to_string(),
            )
        })
        .collect::<HashMap<_, _>>();
    serde_json::to_string_pretty(&values).unwrap_or_else(|_| "{}".to_string())
}

fn sanitize_identifier_component(value: &str) -> String {
    let sanitized = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_uppercase()
            } else {
                '-'
            }
        })
        .collect::<String>();
    sanitized.trim_matches('-').to_string()
}

pub(crate) fn webhook_router(
    command_tx: mpsc::UnboundedSender<RuntimeCommand>,
    config: SharedWebhooksConfig,
) -> Router {
    let state = WebhookState {
        command_tx,
        config,
        delivery_counts: Arc::new(Mutex::new(HashMap::new())),
    };

    Router::new()
        .route("/webhooks/{provider}", post(handle_refresh_webhook))
        .route(
            "/webhooks/triggers/{trigger_id}",
            post(handle_trigger_webhook),
        )
        .with_state(state)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use axum::{body::Body, http::Request};
    use polyphony_workflow::WebhookProviderConfig;
    use tower::ServiceExt;

    use super::*;

    type TestHmac = Hmac<Sha256>;

    fn compute_hmac(secret: &str, body: &[u8]) -> String {
        let mut mac = TestHmac::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body);
        let result = mac.finalize().into_bytes();
        format!("sha256={}", hex::encode(result))
    }

    fn compute_hmac_raw(secret: &str, body: &[u8]) -> String {
        let mut mac = TestHmac::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body);
        let result = mac.finalize().into_bytes();
        hex::encode(result)
    }

    fn config_handle(config: WebhooksConfig) -> SharedWebhooksConfig {
        Arc::new(RwLock::new(config))
    }

    #[test]
    fn hmac_sha256_valid_signature() {
        let strategy = HmacSha256Strategy {
            secret: "mysecret".into(),
        };
        let body = b"hello world";
        let sig = compute_hmac("mysecret", body);
        let mut headers = HeaderMap::new();
        headers.insert("x-hub-signature-256", sig.parse().unwrap());
        assert!(strategy.verify(&headers, &HashMap::new(), body));
    }

    #[test]
    fn query_token_valid() {
        let strategy = QueryTokenStrategy {
            token: "top-secret".into(),
            query_name: "token".into(),
        };
        let query = HashMap::from([("token".into(), "top-secret".into())]);
        assert!(strategy.verify(&HeaderMap::new(), &query, b""));
    }

    #[test]
    fn source_allowlist_matches_ip_and_cidr() {
        assert!(source_allowlist_matches(
            &["192.168.1.1".into(), "10.0.0.0/8".into()],
            "10.2.3.4".parse().unwrap()
        ));
        assert!(!source_allowlist_matches(
            &["192.168.1.1".into(), "10.0.0.0/8".into()],
            "172.16.0.1".parse().unwrap()
        ));
    }

    #[tokio::test]
    async fn webhook_router_accepts_valid_hmac_refresh_provider() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let router = webhook_router(
            tx,
            config_handle(WebhooksConfig {
                enabled: true,
                providers: HashMap::from([("github".into(), WebhookProviderConfig {
                    auth: "hmac_sha256".into(),
                    secret: "test-secret".into(),
                    header: None,
                    query: None,
                    source_allowlist: Vec::new(),
                })]),
                triggers: HashMap::new(),
            }),
        );
        let body_bytes = b"{\"action\":\"opened\"}";
        let sig = compute_hmac("test-secret", body_bytes);

        let request = Request::builder()
            .method("POST")
            .uri("/webhooks/github")
            .header("x-hub-signature-256", sig)
            .extension(ConnectInfo("127.0.0.1:9000".parse::<SocketAddr>().unwrap()))
            .body(Body::from(body_bytes.to_vec()))
            .unwrap();

        let response = router.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(matches!(rx.try_recv(), Ok(RuntimeCommand::Refresh)));
    }

    #[tokio::test]
    async fn webhook_router_dispatches_trigger_webhook() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let router = webhook_router(
            tx,
            config_handle(WebhooksConfig {
                enabled: true,
                providers: HashMap::new(),
                triggers: HashMap::from([("deploy".into(), WebhookTriggerConfig {
                    enabled: true,
                    description: Some("Deploy preview".into()),
                    repo_id: None,
                    agent: "implementer".into(),
                    model: Some("gpt-5.4".into()),
                    auth: "query_token".into(),
                    secret: "query-secret".into(),
                    header: None,
                    query: Some("token".into()),
                    source_allowlist: Vec::new(),
                    title: Some("Deploy {{ delivery_index }}".into()),
                    prompt: "Body:\n{{ body_json }}".into(),
                })]),
            }),
        );

        let request = Request::builder()
            .method("POST")
            .uri("/webhooks/triggers/deploy?token=query-secret")
            .header("content-type", "application/json")
            .extension(ConnectInfo("127.0.0.1:9100".parse::<SocketAddr>().unwrap()))
            .body(Body::from(br#"{"event":"push"}"#.to_vec()))
            .unwrap();

        let response = router.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let command = rx.try_recv().expect("dispatch command");
        match command {
            RuntimeCommand::DispatchWebhook(request) => {
                assert_eq!(request.trigger_id, "deploy");
                assert_eq!(request.agent_name, "implementer");
                assert_eq!(request.model.as_deref(), Some("gpt-5.4"));
                assert!(request.issue.id.starts_with("webhook:deploy:1:"));
                assert_eq!(request.issue.title, "Deploy 1");
                assert!(request.prompt.contains("\"event\": \"push\""));
            },
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[tokio::test]
    async fn webhook_router_rejects_trigger_source_outside_allowlist() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let router = webhook_router(
            tx,
            config_handle(WebhooksConfig {
                enabled: true,
                providers: HashMap::new(),
                triggers: HashMap::from([("deploy".into(), WebhookTriggerConfig {
                    enabled: true,
                    description: None,
                    repo_id: None,
                    agent: "implementer".into(),
                    model: None,
                    auth: "none".into(),
                    secret: String::new(),
                    header: None,
                    query: None,
                    source_allowlist: vec!["192.168.0.0/16".into()],
                    title: None,
                    prompt: "Run".into(),
                })]),
            }),
        );

        let request = Request::builder()
            .method("POST")
            .uri("/webhooks/triggers/deploy")
            .extension(ConnectInfo("10.0.0.5:9100".parse::<SocketAddr>().unwrap()))
            .body(Body::from(Vec::new()))
            .unwrap();

        let response = router.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn build_strategy_token_header_uses_custom_header() {
        let strategy = build_strategy("token_header", "secret", Some("X-Gitlab-Token"), None)
            .expect("strategy");
        let mut headers = HeaderMap::new();
        headers.insert("X-Gitlab-Token", "secret".parse().unwrap());
        assert!(strategy.verify(&headers, &HashMap::new(), b""));
    }

    #[test]
    fn build_strategy_linear_hmac_header_validates() {
        let strategy = build_strategy(
            "hmac_sha256_header",
            "linear-secret",
            Some("Linear-Signature"),
            None,
        )
        .expect("strategy");
        let body = br#"{"action":"create"}"#;
        let sig = compute_hmac_raw("linear-secret", body);
        let mut headers = HeaderMap::new();
        headers.insert("Linear-Signature", sig.parse().unwrap());
        assert!(strategy.verify(&headers, &HashMap::new(), body));
    }
}
