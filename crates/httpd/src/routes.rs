use std::{path::PathBuf, sync::Arc};

use async_graphql_axum::{GraphQLRequest, GraphQLResponse, GraphQLSubscription};
use axum::{
    Form, Router,
    extract::{Query, State},
    http::StatusCode,
    response::{Html, IntoResponse, Redirect},
    routing::{get, post},
};
use axum_login::AuthManagerLayer;
use minijinja::Environment;
use polyphony_core::RuntimeSnapshot;
use polyphony_orchestrator::RuntimeCommand;
use polyphony_workflow::{WebhookTriggerConfig, WebhooksConfig};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, watch};
use tower_http::services::ServeDir;
use tower_sessions::SessionManagerLayer;
use tower_sessions_sqlx_store::SqliteStore;

use crate::{auth, graphql, templates, webhooks};

#[derive(Clone)]
struct AppState {
    schema: graphql::PolyphonySchema,
    snapshot_rx: watch::Receiver<RuntimeSnapshot>,
    template_env: Arc<Environment<'static>>,
    workflow_path: PathBuf,
    auth_backend: Option<auth::Backend>,
    webhooks_config: webhooks::SharedWebhooksConfig,
}

/// Build the full httpd router.
///
/// When `auth_layer` is `Some`, dashboard pages require login.
/// Webhook routes are always outside the auth layer (they have their own auth).
pub fn build_router(
    snapshot_rx: watch::Receiver<RuntimeSnapshot>,
    command_tx: mpsc::UnboundedSender<RuntimeCommand>,
    template_dir: PathBuf,
    workflow_path: PathBuf,
    auth_backend: Option<auth::Backend>,
    auth_layer: Option<AuthManagerLayer<auth::Backend, SqliteStore>>,
    session_layer: Option<SessionManagerLayer<SqliteStore>>,
    webhooks_config: Option<WebhooksConfig>,
) -> Router {
    let schema = graphql::build_schema(snapshot_rx.clone(), command_tx.clone());
    let template_env = Arc::new(templates::build_env(&template_dir));
    let webhooks_config = webhooks_config.unwrap_or_default();
    let shared_webhooks_config = std::sync::Arc::new(tokio::sync::RwLock::new(webhooks_config));

    let state = AppState {
        schema: schema.clone(),
        snapshot_rx,
        template_env: template_env.clone(),
        workflow_path,
        auth_backend,
        webhooks_config: shared_webhooks_config.clone(),
    };

    // Static file serving (CSS, JS, etc.)
    let static_dir = template_dir
        .parent()
        .map(|p| p.join("static"))
        .unwrap_or_else(|| template_dir.join("../static"));

    // SSR + GraphQL pages
    let dashboard_routes = Router::new()
        .nest_service("/static", ServeDir::new(static_dir))
        .route("/", get(page_index))
        .route("/inbox", get(page_inbox))
        .route("/runs", get(page_runs))
        .route("/agents", get(page_agents))
        .route("/outcomes", get(page_outcomes))
        .route("/tasks", get(page_tasks))
        .route("/repos", get(page_repos))
        .route("/webhooks", get(page_webhooks))
        .route("/webhooks/create", post(create_webhook))
        .route("/webhooks/update", post(update_webhook))
        .route("/webhooks/delete", post(delete_webhook))
        .route("/users", get(page_users))
        .route("/users/create", post(create_user))
        .route("/users/update", post(update_user))
        .route("/users/delete", post(delete_user))
        .route("/docs", get(page_docs))
        .route("/logs", get(page_logs))
        .route("/heartbeat", get(page_heartbeat))
        .route("/graphql", get(graphql_playground).post(graphql_handler))
        .route_service("/graphql/ws", GraphQLSubscription::new(schema))
        .with_state(state);

    // Login/logout routes (always public)
    let login_routes = Router::new()
        .route("/login", get(auth::login_page).post(auth::login_submit))
        .route("/logout", get(auth::logout))
        .with_state(template_env);

    // Webhook routes (own auth, outside dashboard auth)
    let webhook_routes = webhooks::webhook_router(command_tx, shared_webhooks_config);

    let mut app = Router::new();

    if let (Some(auth_layer), Some(session_layer)) = (auth_layer, session_layer) {
        // With auth: protect dashboard, keep login + webhooks public
        let protected = dashboard_routes.route_layer(axum_login::login_required!(
            auth::Backend,
            login_url = "/login"
        ));
        app = app
            .merge(protected)
            .merge(login_routes)
            .merge(webhook_routes)
            .layer(auth_layer)
            .layer(session_layer);
    } else {
        // No auth: all routes open, login page not needed
        app = app.merge(dashboard_routes).merge(webhook_routes);
    }

    app
}

// ---------------------------------------------------------------------------
// SSR page handlers
// ---------------------------------------------------------------------------

async fn page_index(State(state): State<AppState>) -> impl IntoResponse {
    render_page(&state, "index.html")
}

async fn page_inbox(State(state): State<AppState>) -> impl IntoResponse {
    render_page(&state, "inbox.html")
}

async fn page_runs(State(state): State<AppState>) -> impl IntoResponse {
    render_page(&state, "runs.html")
}

async fn page_agents(State(state): State<AppState>) -> impl IntoResponse {
    render_page(&state, "agents.html")
}

async fn page_outcomes(State(state): State<AppState>) -> impl IntoResponse {
    render_page(&state, "outcomes.html")
}

async fn page_tasks(State(state): State<AppState>) -> impl IntoResponse {
    render_page(&state, "tasks.html")
}

async fn page_repos(State(state): State<AppState>) -> impl IntoResponse {
    render_page(&state, "repos.html")
}

async fn page_webhooks(
    State(state): State<AppState>,
    Query(query): Query<WebhooksPageQuery>,
) -> impl IntoResponse {
    render_webhooks_page(&state, query, None, StatusCode::OK).await
}

async fn page_docs(State(state): State<AppState>) -> Result<Html<String>, (StatusCode, String)> {
    let snapshot = state.snapshot_rx.borrow().clone();
    let webhooks_config = state.webhooks_config.read().await.clone();

    let providers: Vec<serde_json::Value> = if webhooks_config.enabled {
        webhooks_config
            .providers
            .iter()
            .map(|(name, p)| {
                serde_json::json!({
                    "name": name,
                    "auth": p.auth,
                    "header": p.header,
                    "has_secret": !p.secret.is_empty(),
                    "endpoint": format!("/webhooks/{name}"),
                })
            })
            .collect()
    } else {
        Vec::new()
    };

    let webhooks_enabled = webhooks_config.enabled;

    let ctx = serde_json::json!({
        "webhooks_enabled": webhooks_enabled,
        "webhook_providers": providers,
        "dispatch_mode": snapshot.dispatch_mode,
        "generated_at": snapshot.generated_at,
    });

    let tmpl = state.template_env.get_template("docs.html").map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("template error: {e}"),
        )
    })?;
    let rendered = tmpl
        .render(minijinja::Value::from_serialize(&ctx))
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("render error: {e}"),
            )
        })?;
    Ok(Html(rendered))
}

async fn page_logs(State(state): State<AppState>) -> impl IntoResponse {
    render_page(&state, "logs.html")
}

async fn page_heartbeat(State(state): State<AppState>) -> impl IntoResponse {
    render_page(&state, "heartbeat.html")
}

async fn page_users(
    State(state): State<AppState>,
    Query(query): Query<UsersPageQuery>,
) -> impl IntoResponse {
    render_users_page(&state, query, None, StatusCode::OK)
}

async fn create_webhook(
    State(state): State<AppState>,
    Form(form): Form<CreateWebhookForm>,
) -> impl IntoResponse {
    let mut triggers = match load_webhook_triggers(&state).await {
        Ok(triggers) => triggers,
        Err(error) => {
            return render_webhooks_page(
                &state,
                WebhooksPageQuery::default(),
                Some(error),
                StatusCode::BAD_REQUEST,
            )
            .await;
        },
    };
    let id = match sanitize_webhook_id(&form.id) {
        Ok(id) => id,
        Err(error) => {
            return render_webhooks_page(
                &state,
                WebhooksPageQuery::default(),
                Some(error),
                StatusCode::BAD_REQUEST,
            )
            .await;
        },
    };
    if triggers.contains_key(&id) {
        return render_webhooks_page(
            &state,
            WebhooksPageQuery::default(),
            Some(format!("webhook trigger `{id}` already exists")),
            StatusCode::BAD_REQUEST,
        )
        .await;
    }
    let trigger = match webhook_trigger_from_create_form(&form) {
        Ok(trigger) => trigger,
        Err(error) => {
            return render_webhooks_page(
                &state,
                WebhooksPageQuery::default(),
                Some(error),
                StatusCode::BAD_REQUEST,
            )
            .await;
        },
    };
    if let Err(error) = validate_webhook_agent(&state, &trigger.agent) {
        return render_webhooks_page(
            &state,
            WebhooksPageQuery::default(),
            Some(error),
            StatusCode::BAD_REQUEST,
        )
        .await;
    }
    triggers.insert(id, trigger);
    if let Err(error) = persist_webhook_triggers(&state, triggers).await {
        return render_webhooks_page(
            &state,
            WebhooksPageQuery::default(),
            Some(error),
            StatusCode::BAD_REQUEST,
        )
        .await;
    }
    Redirect::to("/webhooks?result=created").into_response()
}

async fn update_webhook(
    State(state): State<AppState>,
    Form(form): Form<UpdateWebhookForm>,
) -> impl IntoResponse {
    let mut triggers = match load_webhook_triggers(&state).await {
        Ok(triggers) => triggers,
        Err(error) => {
            return render_webhooks_page(
                &state,
                WebhooksPageQuery::default(),
                Some(error),
                StatusCode::BAD_REQUEST,
            )
            .await;
        },
    };
    let original_id = match sanitize_webhook_id(&form.original_id) {
        Ok(id) => id,
        Err(error) => {
            return render_webhooks_page(
                &state,
                WebhooksPageQuery::default(),
                Some(error),
                StatusCode::BAD_REQUEST,
            )
            .await;
        },
    };
    if !triggers.contains_key(&original_id) {
        return render_webhooks_page(
            &state,
            WebhooksPageQuery::default(),
            Some(format!("webhook trigger `{original_id}` was not found")),
            StatusCode::BAD_REQUEST,
        )
        .await;
    }
    let updated_id = match sanitize_webhook_id(&form.id) {
        Ok(id) => id,
        Err(error) => {
            return render_webhooks_page(
                &state,
                WebhooksPageQuery::default(),
                Some(error),
                StatusCode::BAD_REQUEST,
            )
            .await;
        },
    };
    if updated_id != original_id && triggers.contains_key(&updated_id) {
        return render_webhooks_page(
            &state,
            WebhooksPageQuery::default(),
            Some(format!("webhook trigger `{updated_id}` already exists")),
            StatusCode::BAD_REQUEST,
        )
        .await;
    }
    let existing_trigger = triggers.get(&original_id).cloned().expect("checked above");
    let trigger = match webhook_trigger_from_update_form(&form, &existing_trigger) {
        Ok(trigger) => trigger,
        Err(error) => {
            return render_webhooks_page(
                &state,
                WebhooksPageQuery::default(),
                Some(error),
                StatusCode::BAD_REQUEST,
            )
            .await;
        },
    };
    if let Err(error) = validate_webhook_agent(&state, &trigger.agent) {
        return render_webhooks_page(
            &state,
            WebhooksPageQuery::default(),
            Some(error),
            StatusCode::BAD_REQUEST,
        )
        .await;
    }
    triggers.remove(&original_id);
    triggers.insert(updated_id, trigger);
    if let Err(error) = persist_webhook_triggers(&state, triggers).await {
        return render_webhooks_page(
            &state,
            WebhooksPageQuery::default(),
            Some(error),
            StatusCode::BAD_REQUEST,
        )
        .await;
    }
    Redirect::to("/webhooks?result=updated").into_response()
}

async fn delete_webhook(
    State(state): State<AppState>,
    Form(form): Form<DeleteWebhookForm>,
) -> impl IntoResponse {
    let mut triggers = match load_webhook_triggers(&state).await {
        Ok(triggers) => triggers,
        Err(error) => {
            return render_webhooks_page(
                &state,
                WebhooksPageQuery::default(),
                Some(error),
                StatusCode::BAD_REQUEST,
            )
            .await;
        },
    };
    let id = match sanitize_webhook_id(&form.id) {
        Ok(id) => id,
        Err(error) => {
            return render_webhooks_page(
                &state,
                WebhooksPageQuery::default(),
                Some(error),
                StatusCode::BAD_REQUEST,
            )
            .await;
        },
    };
    if triggers.remove(&id).is_none() {
        return render_webhooks_page(
            &state,
            WebhooksPageQuery::default(),
            Some(format!("webhook trigger `{id}` was not found")),
            StatusCode::BAD_REQUEST,
        )
        .await;
    }
    if let Err(error) = persist_webhook_triggers(&state, triggers).await {
        return render_webhooks_page(
            &state,
            WebhooksPageQuery::default(),
            Some(error),
            StatusCode::BAD_REQUEST,
        )
        .await;
    }
    Redirect::to("/webhooks?result=deleted").into_response()
}

async fn create_user(
    State(state): State<AppState>,
    Form(form): Form<CreateUserForm>,
) -> impl IntoResponse {
    let mut users = match load_dashboard_users(&state) {
        Ok(users) => users,
        Err(error) => {
            return render_users_page(
                &state,
                UsersPageQuery::default(),
                Some(error),
                StatusCode::BAD_REQUEST,
            );
        },
    };
    match prepare_user_for_create(&form) {
        Ok(user) => users.push(user),
        Err(error) => {
            return render_users_page(
                &state,
                UsersPageQuery::default(),
                Some(error),
                StatusCode::BAD_REQUEST,
            );
        },
    }
    if let Err(error) = persist_dashboard_users(&state, users).await {
        return render_users_page(
            &state,
            UsersPageQuery::default(),
            Some(error),
            StatusCode::BAD_REQUEST,
        );
    }
    Redirect::to("/users?result=created").into_response()
}

async fn update_user(
    State(state): State<AppState>,
    Form(form): Form<UpdateUserForm>,
) -> impl IntoResponse {
    let mut users = match load_dashboard_users(&state) {
        Ok(users) => users,
        Err(error) => {
            return render_users_page(
                &state,
                UsersPageQuery::default(),
                Some(error),
                StatusCode::BAD_REQUEST,
            );
        },
    };
    let original = form.original_username.trim();
    let Some(index) = users
        .iter()
        .position(|user| usernames_match(&user.username, original))
    else {
        return render_users_page(
            &state,
            UsersPageQuery::default(),
            Some(format!("dashboard user '{original}' was not found")),
            StatusCode::BAD_REQUEST,
        );
    };
    match prepare_user_for_update(&form, &users[index]) {
        Ok(user) => users[index] = user,
        Err(error) => {
            return render_users_page(
                &state,
                UsersPageQuery::default(),
                Some(error),
                StatusCode::BAD_REQUEST,
            );
        },
    }
    if let Err(error) = persist_dashboard_users(&state, users).await {
        return render_users_page(
            &state,
            UsersPageQuery::default(),
            Some(error),
            StatusCode::BAD_REQUEST,
        );
    }
    Redirect::to("/users?result=updated").into_response()
}

async fn delete_user(
    State(state): State<AppState>,
    Form(form): Form<DeleteUserForm>,
) -> impl IntoResponse {
    let mut users = match load_dashboard_users(&state) {
        Ok(users) => users,
        Err(error) => {
            return render_users_page(
                &state,
                UsersPageQuery::default(),
                Some(error),
                StatusCode::BAD_REQUEST,
            );
        },
    };
    let before = users.len();
    users.retain(|user| !usernames_match(&user.username, form.username.trim()));
    if users.len() == before {
        return render_users_page(
            &state,
            UsersPageQuery::default(),
            Some(format!(
                "dashboard user '{}' was not found",
                form.username.trim()
            )),
            StatusCode::BAD_REQUEST,
        );
    }
    if let Err(error) = persist_dashboard_users(&state, users).await {
        return render_users_page(
            &state,
            UsersPageQuery::default(),
            Some(error),
            StatusCode::BAD_REQUEST,
        );
    }
    Redirect::to("/users?result=deleted").into_response()
}

fn render_page(
    state: &AppState,
    template_name: &str,
) -> Result<Html<String>, (StatusCode, String)> {
    let mut snapshot = state.snapshot_rx.borrow().clone();
    // Enrich snapshot with repo registry data (read from disk)
    let registry_path = dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".polyphony")
        .join("repos.json");
    if let Ok(registry) = polyphony_core::load_repo_registry(&registry_path) {
        snapshot.repo_registrations = registry.repos;
    }
    let ctx = templates::snapshot_context(&snapshot);
    let tmpl = state
        .template_env
        .get_template(template_name)
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("template error: {e}"),
            )
        })?;
    let rendered = tmpl.render(ctx).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("render error: {e}"),
        )
    })?;
    Ok(Html(rendered))
}

fn render_users_page(
    state: &AppState,
    query: UsersPageQuery,
    error: Option<String>,
    status: StatusCode,
) -> axum::response::Response {
    let mut snapshot = state.snapshot_rx.borrow().clone();
    let registry_path = polyphony_core::default_repo_registry_path();
    if let Ok(registry) = polyphony_core::load_repo_registry(&registry_path) {
        snapshot.repo_registrations = registry.repos;
    }

    let users = match load_dashboard_users(state) {
        Ok(users) => users,
        Err(load_error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to load dashboard users: {load_error}"),
            )
                .into_response();
        },
    };

    let mut ctx = templates::snapshot_context_object(&snapshot);
    ctx.insert(
        "users".into(),
        serde_json::to_value(&users).unwrap_or_else(|_| serde_json::Value::Array(Vec::new())),
    );
    ctx.insert(
        "auth_live".into(),
        serde_json::Value::Bool(state.auth_backend.is_some()),
    );
    ctx.insert(
        "legacy_mode".into(),
        serde_json::Value::Bool(users.iter().any(|user| user.source == "legacy")),
    );
    ctx.insert(
        "success_message".into(),
        serde_json::Value::String(success_message(&query).unwrap_or_default().to_string()),
    );
    ctx.insert(
        "error_message".into(),
        serde_json::Value::String(error.unwrap_or_default()),
    );

    let tmpl = match state.template_env.get_template("users.html") {
        Ok(tmpl) => tmpl,
        Err(template_error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("template error: {template_error}"),
            )
                .into_response();
        },
    };
    match tmpl.render(minijinja::Value::from_serialize(ctx)) {
        Ok(rendered) => (status, Html(rendered)).into_response(),
        Err(render_error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("render error: {render_error}"),
        )
            .into_response(),
    }
}

async fn render_webhooks_page(
    state: &AppState,
    query: WebhooksPageQuery,
    error: Option<String>,
    status: StatusCode,
) -> axum::response::Response {
    let mut snapshot = state.snapshot_rx.borrow().clone();
    let registry_path = polyphony_core::default_repo_registry_path();
    if let Ok(registry) = polyphony_core::load_repo_registry(&registry_path) {
        snapshot.repo_registrations = registry.repos;
    }

    let config = state.webhooks_config.read().await.clone();
    let mut triggers = config
        .triggers
        .iter()
        .map(|(id, trigger)| WebhookTriggerRow::from_config(id, trigger))
        .collect::<Vec<_>>();
    triggers.sort_by(|left, right| left.id.cmp(&right.id));

    let repo_options = snapshot
        .repo_registrations
        .iter()
        .map(|repo| repo.repo_id.clone())
        .collect::<Vec<_>>();
    let agent_options = if snapshot.agent_profiles.is_empty() {
        snapshot.agent_profile_names.clone()
    } else {
        snapshot
            .agent_profiles
            .iter()
            .map(|profile| profile.name.clone())
            .collect()
    };

    let mut ctx = templates::snapshot_context_object(&snapshot);
    ctx.insert(
        "triggers".into(),
        serde_json::to_value(&triggers).unwrap_or_else(|_| serde_json::Value::Array(Vec::new())),
    );
    ctx.insert(
        "repo_options".into(),
        serde_json::to_value(&repo_options)
            .unwrap_or_else(|_| serde_json::Value::Array(Vec::new())),
    );
    ctx.insert(
        "agent_options".into(),
        serde_json::to_value(&agent_options)
            .unwrap_or_else(|_| serde_json::Value::Array(Vec::new())),
    );
    ctx.insert(
        "providers_count".into(),
        serde_json::Value::from(config.providers.len() as u64),
    );
    ctx.insert(
        "webhooks_enabled".into(),
        serde_json::Value::Bool(config.enabled),
    );
    ctx.insert(
        "success_message".into(),
        serde_json::Value::String(
            webhook_success_message(&query)
                .unwrap_or_default()
                .to_string(),
        ),
    );
    ctx.insert(
        "error_message".into(),
        serde_json::Value::String(error.unwrap_or_default()),
    );

    let tmpl = match state.template_env.get_template("webhooks.html") {
        Ok(tmpl) => tmpl,
        Err(template_error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("template error: {template_error}"),
            )
                .into_response();
        },
    };
    match tmpl.render(minijinja::Value::from_serialize(ctx)) {
        Ok(rendered) => (status, Html(rendered)).into_response(),
        Err(render_error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("render error: {render_error}"),
        )
            .into_response(),
    }
}

async fn load_webhook_triggers(
    state: &AppState,
) -> Result<std::collections::BTreeMap<String, WebhookTriggerConfig>, String> {
    let config = state.webhooks_config.read().await.clone();
    Ok(config.triggers.into_iter().collect())
}

fn validate_webhook_agent(state: &AppState, agent_name: &str) -> Result<(), String> {
    let user_config_path = polyphony_workflow::user_config_path().ok();
    let workflow = polyphony_workflow::load_workflow_with_user_config(
        &state.workflow_path,
        user_config_path.as_deref(),
    )
    .map_err(|error| format!("loading workflow: {error}"))?;
    if workflow.config.agents.profiles.contains_key(agent_name) {
        Ok(())
    } else {
        Err(format!(
            "agent `{agent_name}` is not defined in the loaded workflow"
        ))
    }
}

async fn persist_webhook_triggers(
    state: &AppState,
    triggers: std::collections::BTreeMap<String, WebhookTriggerConfig>,
) -> Result<(), String> {
    let updated =
        polyphony_workflow::update_daemon_config_in_workflow(&state.workflow_path, |daemon| {
            daemon.webhooks.triggers = triggers.clone().into_iter().collect();
            daemon.webhooks.enabled =
                !daemon.webhooks.providers.is_empty() || !daemon.webhooks.triggers.is_empty();
        })
        .map_err(|error| format!("saving daemon config: {error}"))?;

    let mut config = state.webhooks_config.write().await;
    *config = updated.webhooks;
    Ok(())
}

fn load_dashboard_users(state: &AppState) -> Result<Vec<DashboardUserRow>, String> {
    let daemon = polyphony_workflow::load_daemon_config_from_workflow(&state.workflow_path)
        .map_err(|error| format!("loading daemon config: {error}"))?;
    Ok(effective_dashboard_users(&daemon))
}

async fn persist_dashboard_users(
    state: &AppState,
    mut users: Vec<DashboardUserRow>,
) -> Result<(), String> {
    validate_dashboard_users(&users)?;
    users.sort_by(|left, right| {
        normalized_username(&left.username).cmp(&normalized_username(&right.username))
    });

    let daemon =
        polyphony_workflow::update_daemon_config_in_workflow(&state.workflow_path, |daemon| {
            daemon.auth_token = None;
            daemon.users = users
                .iter()
                .map(|user| polyphony_workflow::DaemonUserConfig {
                    username: user.username.clone(),
                    token: user.token.clone(),
                })
                .collect();
        })
        .map_err(|error| format!("saving daemon config: {error}"))?;

    if let Some(backend) = &state.auth_backend {
        let reloaded_users = effective_dashboard_users(&daemon)
            .into_iter()
            .map(|user| auth::User::new(user.username, user.token))
            .collect();
        backend.replace_users(reloaded_users).await;
    }

    Ok(())
}

fn effective_dashboard_users(config: &polyphony_workflow::DaemonConfig) -> Vec<DashboardUserRow> {
    let mut users = if !config.users.is_empty() {
        config
            .users
            .iter()
            .map(|user| DashboardUserRow::new(user.username.clone(), user.token.clone(), "user"))
            .collect::<Vec<_>>()
    } else {
        config
            .auth_token
            .clone()
            .filter(|token| !token.trim().is_empty())
            .map(|token| vec![DashboardUserRow::new("admin".into(), token, "legacy")])
            .unwrap_or_default()
    };
    users.sort_by(|left, right| {
        normalized_username(&left.username).cmp(&normalized_username(&right.username))
    });
    users
}

fn prepare_user_for_create(form: &CreateUserForm) -> Result<DashboardUserRow, String> {
    Ok(DashboardUserRow::new(
        sanitize_username(&form.username)?,
        sanitize_token(&form.token)?,
        "user",
    ))
}

fn prepare_user_for_update(
    form: &UpdateUserForm,
    current_user: &DashboardUserRow,
) -> Result<DashboardUserRow, String> {
    let token = form
        .token
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(sanitize_token)
        .transpose()?
        .unwrap_or_else(|| current_user.token.clone());
    Ok(DashboardUserRow::new(
        sanitize_username(&form.username)?,
        token,
        "user",
    ))
}

fn validate_dashboard_users(users: &[DashboardUserRow]) -> Result<(), String> {
    if users.is_empty() {
        return Err(
            "refusing to remove the last dashboard user; edit WORKFLOW.md directly if you want to disable auth"
                .into(),
        );
    }
    for user in users {
        sanitize_username(&user.username)?;
        sanitize_token(&user.token)?;
    }
    for (index, user) in users.iter().enumerate() {
        let duplicate = users
            .iter()
            .skip(index + 1)
            .any(|other| usernames_match(&user.username, &other.username));
        if duplicate {
            return Err(format!(
                "dashboard username '{}' already exists",
                user.username.trim()
            ));
        }
    }
    Ok(())
}

fn sanitize_username(username: &str) -> Result<String, String> {
    let username = username.trim();
    if username.is_empty() {
        return Err("username cannot be empty".into());
    }
    Ok(username.to_string())
}

fn sanitize_token(token: &str) -> Result<String, String> {
    let token = token.trim();
    if token.is_empty() {
        return Err("token cannot be empty".into());
    }
    Ok(token.to_string())
}

fn usernames_match(left: &str, right: &str) -> bool {
    normalized_username(left) == normalized_username(right)
}

fn normalized_username(username: &str) -> String {
    username.trim().to_ascii_lowercase()
}

fn success_message(query: &UsersPageQuery) -> Option<&'static str> {
    match query.result.as_deref() {
        Some("created") => Some("Dashboard user added."),
        Some("updated") => Some("Dashboard user updated."),
        Some("deleted") => Some("Dashboard user removed."),
        _ => None,
    }
}

fn webhook_success_message(query: &WebhooksPageQuery) -> Option<&'static str> {
    match query.result.as_deref() {
        Some("created") => Some("Webhook trigger saved."),
        Some("updated") => Some("Webhook trigger updated."),
        Some("deleted") => Some("Webhook trigger removed."),
        _ => None,
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
struct UsersPageQuery {
    result: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CreateUserForm {
    username: String,
    token: String,
}

#[derive(Debug, Deserialize)]
struct UpdateUserForm {
    original_username: String,
    username: String,
    token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DeleteUserForm {
    username: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct WebhooksPageQuery {
    result: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CreateWebhookForm {
    id: String,
    enabled: Option<String>,
    description: Option<String>,
    repo_id: Option<String>,
    agent: String,
    model: Option<String>,
    auth: String,
    secret: String,
    header: Option<String>,
    query: Option<String>,
    source_allowlist: Option<String>,
    title: Option<String>,
    prompt: String,
}

#[derive(Debug, Deserialize)]
struct UpdateWebhookForm {
    original_id: String,
    id: String,
    enabled: Option<String>,
    description: Option<String>,
    repo_id: Option<String>,
    agent: String,
    model: Option<String>,
    auth: String,
    secret: String,
    header: Option<String>,
    query: Option<String>,
    source_allowlist: Option<String>,
    title: Option<String>,
    prompt: String,
}

#[derive(Debug, Deserialize)]
struct DeleteWebhookForm {
    id: String,
}

#[derive(Debug, Clone, Serialize)]
struct DashboardUserRow {
    username: String,
    #[serde(skip_serializing)]
    token: String,
    masked_token: String,
    source: &'static str,
}

#[derive(Debug, Clone, Serialize)]
struct WebhookTriggerRow {
    id: String,
    enabled: bool,
    description: Option<String>,
    repo_id: Option<String>,
    agent: String,
    model: Option<String>,
    auth: String,
    masked_secret: String,
    header: Option<String>,
    query: Option<String>,
    source_allowlist: Vec<String>,
    source_allowlist_display: String,
    title: Option<String>,
    prompt: String,
    endpoint: String,
}

impl DashboardUserRow {
    fn new(username: String, token: String, source: &'static str) -> Self {
        Self {
            username,
            masked_token: mask_token(&token),
            token,
            source,
        }
    }
}

impl WebhookTriggerRow {
    fn from_config(id: &str, trigger: &WebhookTriggerConfig) -> Self {
        let source_allowlist_display = if trigger.source_allowlist.is_empty() {
            "none".into()
        } else {
            trigger.source_allowlist.join(", ")
        };
        Self {
            id: id.to_string(),
            enabled: trigger.enabled,
            description: trigger.description.clone(),
            repo_id: trigger.repo_id.clone(),
            agent: trigger.agent.clone(),
            model: trigger.model.clone(),
            auth: trigger.auth.clone(),
            masked_secret: mask_token(&trigger.secret),
            header: trigger.header.clone(),
            query: trigger.query.clone(),
            source_allowlist: trigger.source_allowlist.clone(),
            source_allowlist_display,
            title: trigger.title.clone(),
            prompt: trigger.prompt.clone(),
            endpoint: format!("/webhooks/triggers/{id}"),
        }
    }
}

fn webhook_trigger_from_create_form(
    form: &CreateWebhookForm,
) -> Result<WebhookTriggerConfig, String> {
    webhook_trigger_from_fields(
        form.enabled.is_some(),
        form.description.as_deref(),
        form.repo_id.as_deref(),
        &form.agent,
        form.model.as_deref(),
        &form.auth,
        &form.secret,
        form.header.as_deref(),
        form.query.as_deref(),
        form.source_allowlist.as_deref(),
        form.title.as_deref(),
        &form.prompt,
    )
}

fn webhook_trigger_from_update_form(
    form: &UpdateWebhookForm,
    existing: &WebhookTriggerConfig,
) -> Result<WebhookTriggerConfig, String> {
    webhook_trigger_from_fields(
        form.enabled.is_some(),
        form.description.as_deref(),
        form.repo_id.as_deref(),
        &form.agent,
        form.model.as_deref(),
        &form.auth,
        if form.secret.trim().is_empty() {
            &existing.secret
        } else {
            &form.secret
        },
        form.header.as_deref(),
        form.query.as_deref(),
        form.source_allowlist.as_deref(),
        form.title.as_deref(),
        &form.prompt,
    )
}

fn webhook_trigger_from_fields(
    enabled: bool,
    description: Option<&str>,
    repo_id: Option<&str>,
    agent: &str,
    model: Option<&str>,
    auth: &str,
    secret: &str,
    header: Option<&str>,
    query: Option<&str>,
    source_allowlist: Option<&str>,
    title: Option<&str>,
    prompt: &str,
) -> Result<WebhookTriggerConfig, String> {
    let trigger = WebhookTriggerConfig {
        enabled,
        description: sanitize_optional_text(description),
        repo_id: sanitize_optional_text(repo_id),
        agent: sanitize_required_text(agent, "agent")?,
        model: sanitize_optional_text(model),
        auth: sanitize_required_text(auth, "auth")?.to_ascii_lowercase(),
        secret: secret.trim().to_string(),
        header: sanitize_optional_text(header),
        query: sanitize_optional_text(query),
        source_allowlist: parse_source_allowlist(source_allowlist),
        title: sanitize_optional_text(title),
        prompt: sanitize_required_text(prompt, "prompt")?,
    };
    validate_webhook_trigger(&trigger)?;
    Ok(trigger)
}

fn sanitize_webhook_id(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err("webhook id cannot be empty".into());
    }
    if !value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    {
        return Err("webhook id may only contain ASCII letters, digits, `.`, `_`, or `-`".into());
    }
    Ok(value.to_string())
}

fn sanitize_required_text(value: &str, field: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(format!("{field} cannot be empty"));
    }
    Ok(value.to_string())
}

fn sanitize_optional_text(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn parse_source_allowlist(value: Option<&str>) -> Vec<String> {
    value
        .unwrap_or_default()
        .split([',', '\n'])
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn validate_webhook_trigger(trigger: &WebhookTriggerConfig) -> Result<(), String> {
    match trigger.auth.as_str() {
        "hmac_sha256" | "hmac_sha256_header" | "token_header" | "bearer" | "query_token" => {
            if trigger.secret.trim().is_empty() {
                return Err("secret cannot be empty for the selected auth mode".into());
            }
        },
        "none" => {
            if trigger.source_allowlist.is_empty() {
                return Err("auth `none` requires at least one source allowlist entry".into());
            }
        },
        other => {
            return Err(format!(
                "auth `{other}` is not supported, expected hmac_sha256, hmac_sha256_header, token_header, bearer, query_token, or none"
            ));
        },
    }
    if matches!(trigger.auth.as_str(), "token_header" | "hmac_sha256_header")
        && trigger
            .header
            .as_deref()
            .unwrap_or_default()
            .trim()
            .is_empty()
    {
        return Err("header is required for the selected auth mode".into());
    }
    if trigger.auth == "query_token"
        && trigger
            .query
            .as_deref()
            .unwrap_or_default()
            .trim()
            .is_empty()
    {
        return Err("query parameter name is required for query_token auth".into());
    }
    Ok(())
}

fn mask_token(token: &str) -> String {
    let visible = token.chars().count().min(4);
    let suffix: String = token
        .chars()
        .rev()
        .take(visible)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    if token.is_empty() {
        return "empty".into();
    }
    if token.chars().count() <= 4 {
        return "****".into();
    }
    format!("****{}", suffix)
}

// ---------------------------------------------------------------------------
// GraphQL handlers
// ---------------------------------------------------------------------------

async fn graphql_handler(State(state): State<AppState>, req: GraphQLRequest) -> GraphQLResponse {
    state.schema.execute(req.into_inner()).await.into()
}

async fn graphql_playground() -> impl IntoResponse {
    Html(async_graphql::http::playground_source(
        async_graphql::http::GraphQLPlaygroundConfig::new("/graphql")
            .subscription_endpoint("/graphql/ws"),
    ))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use axum::{
        body::{Body, to_bytes},
        http::Request,
    };
    use serde_json::json;
    use tower::ServiceExt;

    use super::*;

    fn test_snapshot() -> RuntimeSnapshot {
        serde_json::from_value(json!({
            "generated_at": "2026-01-01T00:00:00Z",
            "dispatch_mode": "manual",
            "counts": {
                "running": 0,
                "retrying": 0,
                "runs": 0,
                "tasks_pending": 0,
                "tasks_in_progress": 0,
                "tasks_completed": 0,
                "worktrees": 0
            },
            "running": [],
            "retrying": [],
            "codex_totals": { "input_tokens": 0, "output_tokens": 0, "total_tokens": 0, "seconds_running": 0.0 },
            "rate_limits": null,
            "throttles": [],
            "budgets": [],
            "agent_catalogs": [],
            "saved_contexts": [],
            "recent_events": [],
            "repo_registrations": []
        }))
        .expect("snapshot should deserialize")
    }

    fn template_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("templates")
    }

    #[tokio::test]
    async fn users_page_renders_legacy_auth_user() {
        let dir = tempfile::tempdir().expect("tempdir");
        let workflow_path = dir.path().join("WORKFLOW.md");
        std::fs::write(
            &workflow_path,
            "---\ndaemon:\n  auth_token: legacy-secret\n---\nPrompt\n",
        )
        .expect("write workflow");

        let (snapshot_tx, snapshot_rx) = watch::channel(test_snapshot());
        let _ = snapshot_tx;
        let (command_tx, _command_rx) = mpsc::unbounded_channel();
        let router = build_router(
            snapshot_rx,
            command_tx,
            template_dir(),
            workflow_path,
            None,
            None,
            None,
            None,
        );

        let response = router
            .oneshot(
                Request::builder()
                    .uri("/users")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let html = String::from_utf8(body.to_vec()).expect("utf8");
        assert_eq!(status, StatusCode::OK, "{html}");
        assert!(html.contains("admin"));
        assert!(html.contains("legacy"));
        assert!(html.contains("daemon.auth_token"));
        assert!(!html.contains("legacy-secret"));
    }

    #[tokio::test]
    async fn create_user_persists_named_dashboard_users() {
        let dir = tempfile::tempdir().expect("tempdir");
        let workflow_path = dir.path().join("WORKFLOW.md");
        std::fs::write(&workflow_path, "---\ntracker:\n  kind: none\n---\nPrompt\n")
            .expect("write workflow");

        let (snapshot_tx, snapshot_rx) = watch::channel(test_snapshot());
        let _ = snapshot_tx;
        let (command_tx, _command_rx) = mpsc::unbounded_channel();
        let router = build_router(
            snapshot_rx,
            command_tx,
            template_dir(),
            workflow_path.clone(),
            None,
            None,
            None,
            None,
        );

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/users/create")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from("username=alice&token=secret-1"))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::SEE_OTHER);

        let daemon = polyphony_workflow::load_daemon_config_from_workflow(&workflow_path)
            .expect("load daemon");
        assert!(daemon.auth_token.is_none());
        assert_eq!(daemon.users, vec![polyphony_workflow::DaemonUserConfig {
            username: "alice".into(),
            token: "secret-1".into(),
        }]);
    }

    #[tokio::test]
    async fn update_user_keeps_existing_token_when_field_is_blank() {
        let dir = tempfile::tempdir().expect("tempdir");
        let workflow_path = dir.path().join("WORKFLOW.md");
        std::fs::write(
            &workflow_path,
            "---\ndaemon:\n  users:\n    - username: alice\n      token: secret-1\n---\nPrompt\n",
        )
        .expect("write workflow");

        let (snapshot_tx, snapshot_rx) = watch::channel(test_snapshot());
        let _ = snapshot_tx;
        let (command_tx, _command_rx) = mpsc::unbounded_channel();
        let router = build_router(
            snapshot_rx,
            command_tx,
            template_dir(),
            workflow_path.clone(),
            None,
            None,
            None,
            None,
        );

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/users/update")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from(
                        "original_username=alice&username=alice-renamed&token=",
                    ))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::SEE_OTHER);

        let daemon = polyphony_workflow::load_daemon_config_from_workflow(&workflow_path)
            .expect("load daemon");
        assert_eq!(daemon.users.len(), 1);
        assert_eq!(daemon.users[0].username, "alice-renamed");
        assert_eq!(daemon.users[0].token, "secret-1");
    }

    #[tokio::test]
    async fn create_webhook_persists_trigger_definition() {
        let dir = tempfile::tempdir().expect("tempdir");
        let workflow_path = dir.path().join("WORKFLOW.md");
        std::fs::write(
            &workflow_path,
            "---\ntracker:\n  kind: none\nagents:\n  default: implementer\n  profiles:\n    implementer:\n      kind: mock\n      transport: mock\n      command: mock\n---\nPrompt\n",
        )
        .expect("write workflow");

        let (snapshot_tx, snapshot_rx) = watch::channel(test_snapshot());
        let _ = snapshot_tx;
        let (command_tx, _command_rx) = mpsc::unbounded_channel();
        let router = build_router(
            snapshot_rx,
            command_tx,
            template_dir(),
            workflow_path.clone(),
            None,
            None,
            None,
            None,
        );

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/webhooks/create")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from(
                        "id=deploy&enabled=on&agent=implementer&auth=query_token&secret=secret-1&query=token&prompt=Inspect",
                    ))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::SEE_OTHER);

        let daemon = polyphony_workflow::load_daemon_config_from_workflow(&workflow_path)
            .expect("load daemon");
        let trigger = daemon
            .webhooks
            .triggers
            .get("deploy")
            .expect("saved trigger");
        assert!(daemon.webhooks.enabled);
        assert_eq!(trigger.agent, "implementer");
        assert_eq!(trigger.auth, "query_token");
        assert_eq!(trigger.secret, "secret-1");
        assert_eq!(trigger.query.as_deref(), Some("token"));
        assert_eq!(trigger.prompt, "Inspect");
    }

    #[tokio::test]
    async fn update_webhook_keeps_existing_secret_when_field_is_blank() {
        let dir = tempfile::tempdir().expect("tempdir");
        let workflow_path = dir.path().join("WORKFLOW.md");
        std::fs::write(
            &workflow_path,
            "---\ntracker:\n  kind: none\nagents:\n  default: implementer\n  profiles:\n    implementer:\n      kind: mock\n      transport: mock\n      command: mock\ndaemon:\n  webhooks:\n    enabled: true\n    triggers:\n      deploy:\n        enabled: true\n        agent: implementer\n        auth: bearer\n        secret: existing-secret\n        prompt: Inspect\n---\nPrompt\n",
        )
        .expect("write workflow");

        let (snapshot_tx, snapshot_rx) = watch::channel(test_snapshot());
        let _ = snapshot_tx;
        let (command_tx, _command_rx) = mpsc::unbounded_channel();
        let router = build_router(
            snapshot_rx,
            command_tx,
            template_dir(),
            workflow_path.clone(),
            None,
            None,
            None,
            Some(
                polyphony_workflow::load_daemon_config_from_workflow(&workflow_path)
                    .expect("daemon")
                    .webhooks,
            ),
        );

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/webhooks/update")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from(
                        "original_id=deploy&id=deploy&enabled=on&agent=implementer&auth=bearer&secret=&prompt=Inspect+again",
                    ))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::SEE_OTHER);

        let daemon = polyphony_workflow::load_daemon_config_from_workflow(&workflow_path)
            .expect("load daemon");
        let trigger = daemon
            .webhooks
            .triggers
            .get("deploy")
            .expect("saved trigger");
        assert_eq!(trigger.secret, "existing-secret");
        assert_eq!(trigger.prompt, "Inspect again");
    }

    #[test]
    fn validate_dashboard_users_rejects_duplicates() {
        let users = vec![
            DashboardUserRow::new("alice".into(), "secret-1".into(), "user"),
            DashboardUserRow::new("Alice".into(), "secret-2".into(), "user"),
        ];

        let error = validate_dashboard_users(&users).expect_err("duplicate usernames");
        assert!(error.contains("already exists"));
    }
}
