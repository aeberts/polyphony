use std::sync::{Arc, Mutex};

use axum::{
    Json, Router,
    extract::State,
    http::{StatusCode as HttpStatusCode, Uri},
    response::IntoResponse,
    routing::{get, post},
};
use chrono::{TimeZone, Utc};
use graphql_client::GraphQLQuery;
use octocrab::models::AuthorAssociation;
use polyphony_core::{DispatchApprovalState, IssueTracker, TrackerQuery};
use reqwest::{
    StatusCode,
    header::{HeaderMap, HeaderValue, RETRY_AFTER},
};
use serde_json::{Value, json};

use crate::{
    convert::{
        find_status_field_option, github_issue_approval_state, github_rate_limit_signal,
        organization_project_id_from_context, parse_rate_limit_reset, parse_retry_after_ms,
        required_project_issue_status, user_project_id_from_context,
    },
    fetch_pull_request_events,
    pull_requests::{GithubIssueCommentResponse, find_issue_comment_id_with_marker},
    resolve_organization_project_issue_context, resolve_project_status_field,
    resolve_user_project_issue_context,
    review_events::{
        GithubReviewBranchRef, GithubReviewHeadRef, GithubReviewLabel,
        GithubReviewPullRequestResponse, GithubReviewUser,
        pull_request_review_events_from_responses, should_emit_conflict_event,
    },
};

#[derive(Clone)]
struct ProjectStatusMock {
    statuses: Arc<Mutex<Vec<String>>>,
    organization_owned: bool,
    operations: Arc<Mutex<Vec<String>>>,
}

async fn mock_github_issue() -> Json<Value> {
    Json(json!({
        "id": 42,
        "node_id": "I_42",
        "url": "http://example.test/issues/42",
        "repository_url": "http://example.test/repos/repo-owner/repo",
        "labels_url": "http://example.test/issues/42/labels",
        "comments_url": "http://example.test/issues/42/comments",
        "events_url": "http://example.test/issues/42/events",
        "html_url": "http://example.test/issues/42",
        "number": 42,
        "state": "open",
        "state_reason": null,
        "title": "Project-status fixture",
        "body": null,
        "user": {
            "login": "repo-owner", "id": 1, "node_id": "U_1",
            "avatar_url": "http://example.test/avatar", "gravatar_id": "",
            "url": "http://example.test/users/repo-owner",
            "html_url": "http://example.test/repo-owner",
            "followers_url": "http://example.test/followers",
            "following_url": "http://example.test/following",
            "gists_url": "http://example.test/gists",
            "starred_url": "http://example.test/starred",
            "subscriptions_url": "http://example.test/subscriptions",
            "organizations_url": "http://example.test/organizations",
            "repos_url": "http://example.test/repos",
            "events_url": "http://example.test/events",
            "received_events_url": "http://example.test/received-events",
            "type": "User", "site_admin": false, "name": null, "patch_url": null,
            "email": null
        },
        "labels": [], "assignee": null, "assignees": [],
        "author_association": "OWNER", "milestone": null, "locked": false,
        "active_lock_reason": null, "comments": 0, "pull_request": null,
        "closed_at": null, "closed_by": null,
        "created_at": "2025-01-01T00:00:00Z",
        "updated_at": "2025-01-01T00:00:00Z"
    }))
}

async fn mock_github_issues() -> Json<Value> {
    Json(json!([mock_github_issue().await.0]))
}

async fn mock_graphql(
    State(state): State<ProjectStatusMock>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let operation = body["operationName"].as_str().unwrap_or_default();
    state.operations.lock().unwrap().push(operation.to_owned());
    let payload = match operation {
        "ResolveUserProjectIssueContext" if state.organization_owned => json!({"data": {
            "repository": {"issue": {"id": "I_42"}},
            "user": {"projectV2": null}
        }}),
        "ResolveUserProjectIssueContext" => json!({"data": {
            "repository": {"issue": {"id": "I_42"}},
            "user": {"projectV2": {"id": "P_USER"}}
        }}),
        "ResolveOrganizationProjectIssueContext" if state.organization_owned => json!({"data": {
            "repository": {"issue": {"id": "I_42"}},
            "organization": {"projectV2": {"id": "P_ORG"}}
        }}),
        "ResolveProjectIssueStatus" => {
            let status = state.statuses.lock().unwrap().remove(0);
            let project_id = if state.organization_owned {
                "P_ORG"
            } else {
                "P_USER"
            };
            json!({"data": {"repository": {"issue": {"projectItems": {"nodes": [{
                "project": {"id": project_id},
                "fieldValueByName": {
                    "__typename": "ProjectV2ItemFieldSingleSelectValue",
                    "name": status
                }
            }]}}}}})
        },
        unexpected => panic!("unexpected GraphQL operation: {unexpected}"),
    };
    Json(payload)
}

async fn mock_not_found(_uri: Uri) -> impl IntoResponse {
    (HttpStatusCode::NOT_FOUND, "unexpected GitHub mock request")
}

async fn project_status_tracker(statuses: Vec<&str>) -> crate::GithubIssueTracker {
    project_status_tracker_for_owner(statuses, false).await.0
}

async fn project_status_tracker_for_owner(
    statuses: Vec<&str>,
    organization_owned: bool,
) -> (crate::GithubIssueTracker, Arc<Mutex<Vec<String>>>) {
    let operations = Arc::new(Mutex::new(Vec::new()));
    let state = ProjectStatusMock {
        statuses: Arc::new(Mutex::new(
            statuses.into_iter().map(str::to_owned).collect(),
        )),
        organization_owned,
        operations: operations.clone(),
    };
    let app = Router::new()
        .route("/repos/{owner}/{repo}/issues", get(mock_github_issues))
        .route(
            "/repos/{owner}/{repo}/issues/{number}",
            get(mock_github_issue),
        )
        .route("/graphql", post(mock_graphql))
        .fallback(mock_not_found)
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let tracker = crate::GithubIssueTracker::new_for_test(
        "repo-owner/repo".into(),
        if organization_owned {
            "project-org"
        } else {
            "project-user"
        }
        .into(),
        1,
        base_url,
    )
    .unwrap();
    (tracker, operations)
}

#[tokio::test]
async fn tracker_uses_project_status_for_candidate_polling_and_reconciliation() {
    let tracker = project_status_tracker(vec!["Ready", "Backlog"]).await;
    let candidates = tracker
        .fetch_candidate_issues(&TrackerQuery {
            project_slug: None,
            repository: None,
            active_states: vec!["Ready".into()],
            terminal_states: Vec::new(),
        })
        .await
        .unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].state, "Ready");

    let updates = tracker
        .fetch_issue_states_by_ids(&["42".into()])
        .await
        .unwrap();
    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0].state, "Backlog");
    assert_ne!(updates[0].state, "Ready");
}

#[tokio::test]
async fn organization_owned_project_fallback_is_used_for_candidate_and_reconciliation() {
    let (tracker, operations) =
        project_status_tracker_for_owner(vec!["Ready", "Backlog"], true).await;
    let candidates = tracker
        .fetch_candidate_issues(&TrackerQuery {
            project_slug: None,
            repository: None,
            active_states: vec!["Ready".into()],
            terminal_states: Vec::new(),
        })
        .await
        .unwrap();
    assert_eq!(candidates[0].state, "Ready");
    let updates = tracker
        .fetch_issue_states_by_ids(&["42".into()])
        .await
        .unwrap();
    assert_eq!(updates[0].state, "Backlog");
    let operations = operations.lock().unwrap();
    assert_eq!(
        operations
            .iter()
            .filter(|operation| operation.as_str() == "ResolveOrganizationProjectIssueContext")
            .count(),
        2,
        "both tracker paths must use the user-empty to organization fallback"
    );
}

#[tokio::test]
async fn tracker_rejects_empty_project_status_for_candidates_and_reconciliation() {
    let tracker = project_status_tracker(vec!["  ", "\t"]).await;
    let candidate_error = tracker
        .fetch_candidate_issues(&TrackerQuery {
            project_slug: None,
            repository: None,
            active_states: vec!["Ready".into()],
            terminal_states: Vec::new(),
        })
        .await
        .unwrap_err();
    assert!(candidate_error.to_string().contains("missing or empty"));
    assert!(
        candidate_error
            .to_string()
            .contains("refusing to use GitHub open/closed state")
    );

    let reconciliation_error = tracker
        .fetch_issue_states_by_ids(&["42".into()])
        .await
        .unwrap_err();
    assert!(
        reconciliation_error
            .to_string()
            .contains("missing or empty")
    );
}

#[test]
fn user_owned_project_query_never_requests_an_organization() {
    let body = crate::ResolveUserProjectIssueContext::build_query(
        resolve_user_project_issue_context::Variables {
            owner: "repo-owner".into(),
            repo: "repo".into(),
            number: 42,
            project_owner: "aeberts".into(),
            project_number: 1,
        },
    );
    let query = serde_json::to_value(body).unwrap()["query"]
        .as_str()
        .unwrap()
        .to_string();

    assert!(query.contains("user(login: $projectOwner)"));
    assert!(!query.contains("organization(login: $projectOwner)"));
}

#[test]
fn organization_fallback_query_never_requests_a_user() {
    let body = crate::ResolveOrganizationProjectIssueContext::build_query(
        resolve_organization_project_issue_context::Variables {
            owner: "repo-owner".into(),
            repo: "repo".into(),
            number: 42,
            project_owner: "acme".into(),
            project_number: 1,
        },
    );
    let query = serde_json::to_value(body).unwrap()["query"]
        .as_str()
        .unwrap()
        .to_string();

    assert!(query.contains("organization(login: $projectOwner)"));
    assert!(!query.contains("user(login: $projectOwner)"));
}

#[test]
fn user_owned_project_context_resolves_without_an_organization_lookup() {
    let data = resolve_user_project_issue_context::ResponseData {
            repository: None,
            user: Some(resolve_user_project_issue_context::ResolveUserProjectIssueContextUser {
                project_v2: Some(resolve_user_project_issue_context::ResolveUserProjectIssueContextUserProjectV2 {
                    id: "USER_PROJECT".into(),
                }),
            }),
        };
    assert_eq!(
        user_project_id_from_context(&data).as_deref(),
        Some("USER_PROJECT")
    );
}

#[test]
fn organization_owned_project_context_resolves_after_user_lookup_is_empty() {
    let data = resolve_organization_project_issue_context::ResponseData {
        repository: None,
        organization: Some(
            resolve_organization_project_issue_context::ResolveOrganizationProjectIssueContextOrganization {
                project_v2: Some(
                    resolve_organization_project_issue_context::ResolveOrganizationProjectIssueContextOrganizationProjectV2 {
                        id: "ORG_PROJECT".into(),
                    },
                ),
            },
        ),
    };

    assert_eq!(
        organization_project_id_from_context(&data).as_deref(),
        Some("ORG_PROJECT")
    );
}

#[test]
fn missing_project_item_or_status_is_an_error_not_a_todo_fallback() {
    let error = required_project_issue_status(None, "Status", 42).unwrap_err();
    assert!(error.to_string().contains("missing"));
    assert!(
        error
            .to_string()
            .contains("refusing to use GitHub open/closed state")
    );
}

#[test]
fn configured_project_status_is_the_state_used_for_ready_and_backlog_transitions() {
    let ready = required_project_issue_status(Some("Ready".into()), "Status", 42).unwrap();
    let backlog = required_project_issue_status(Some("Backlog".into()), "Status", 42).unwrap();

    assert_eq!(ready, "Ready");
    assert_eq!(backlog, "Backlog");
    assert_ne!(
        backlog, "Ready",
        "an open GitHub issue in Backlog is ineligible for Ready"
    );
}

#[test]
fn finds_status_option_case_insensitively() {
    let nodes = vec![vec![Some(
            resolve_project_status_field::ResolveProjectStatusFieldNodeOnProjectV2FieldsNodes::ProjectV2SingleSelectField(
                resolve_project_status_field::ResolveProjectStatusFieldNodeOnProjectV2FieldsNodesOnProjectV2SingleSelectField {
                    id: "field-1".into(),
                    name: "Status".into(),
                    options: vec![
                        resolve_project_status_field::ResolveProjectStatusFieldNodeOnProjectV2FieldsNodesOnProjectV2SingleSelectFieldOptions {
                            id: "opt-1".into(),
                            name: "Todo".into(),
                        },
                        resolve_project_status_field::ResolveProjectStatusFieldNodeOnProjectV2FieldsNodesOnProjectV2SingleSelectFieldOptions {
                            id: "opt-2".into(),
                            name: "In Progress".into(),
                        },
                        resolve_project_status_field::ResolveProjectStatusFieldNodeOnProjectV2FieldsNodesOnProjectV2SingleSelectFieldOptions {
                            id: "opt-3".into(),
                            name: "Human Review".into(),
                        },
                    ],
                },
            ),
        )]];

    assert_eq!(
        find_status_field_option(&nodes, "status", "human review"),
        Some(("field-1".into(), "opt-3".into()))
    );
}

#[test]
fn retry_after_header_is_converted_to_milliseconds() {
    let mut headers = HeaderMap::new();
    headers.insert(RETRY_AFTER, HeaderValue::from_static("12"));

    assert_eq!(parse_retry_after_ms(&headers), Some(12_000));
}

#[test]
fn reset_header_is_converted_to_utc_timestamp() {
    let mut headers = HeaderMap::new();
    headers.insert("x-ratelimit-reset", HeaderValue::from_static("1710000000"));

    assert_eq!(
        parse_rate_limit_reset(&headers),
        Utc.timestamp_opt(1_710_000_000, 0).single()
    );
}

#[test]
fn secondary_rate_limit_without_headers_falls_back_to_one_minute() {
    let signal = github_rate_limit_signal(
        "tracker:github",
        StatusCode::TOO_MANY_REQUESTS,
        &HeaderMap::new(),
        None,
    )
    .unwrap();

    assert_eq!(signal.retry_after_ms, Some(60_000));
    assert!(signal.reset_at.is_none());
}

#[test]
fn primary_rate_limit_uses_reset_header_instead_of_guessing_retry_after() {
    let mut headers = HeaderMap::new();
    headers.insert("x-ratelimit-remaining", HeaderValue::from_static("0"));
    headers.insert("x-ratelimit-reset", HeaderValue::from_static("1710000000"));

    let signal =
        github_rate_limit_signal("tracker:github", StatusCode::FORBIDDEN, &headers, None).unwrap();

    assert_eq!(signal.retry_after_ms, None);
    assert_eq!(
        signal.reset_at,
        Utc.timestamp_opt(1_710_000_000, 0).single()
    );
}

#[test]
fn pull_request_review_events_keep_fork_heads_and_set_checkout_refs() {
    let events = pull_request_review_events_from_responses("penso/polyphony", vec![
        GithubReviewPullRequestResponse {
            number: 42,
            title: "Ready".into(),
            html_url: "https://github.com/penso/polyphony/pull/42".into(),
            created_at: Utc.timestamp_opt(1_709_999_000, 0).single().unwrap(),
            updated_at: Utc.timestamp_opt(1_710_000_000, 0).single().unwrap(),
            draft: Some(false),
            user: Some(GithubReviewUser {
                login: "alice".into(),
            }),
            author_association: Some(AuthorAssociation::Collaborator),
            labels: vec![GithubReviewLabel {
                name: "Needs Review".into(),
            }],
            base: GithubReviewBranchRef {
                name: "main".into(),
            },
            head: GithubReviewHeadRef {
                name: "feature/review".into(),
                sha: "abc123".into(),
            },
        },
        GithubReviewPullRequestResponse {
            number: 43,
            title: "Fork".into(),
            html_url: "https://github.com/penso/polyphony/pull/43".into(),
            created_at: Utc.timestamp_opt(1_709_999_001, 0).single().unwrap(),
            updated_at: Utc.timestamp_opt(1_710_000_001, 0).single().unwrap(),
            draft: Some(false),
            user: Some(GithubReviewUser {
                login: "dependabot[bot]".into(),
            }),
            author_association: Some(AuthorAssociation::Contributor),
            labels: Vec::new(),
            base: GithubReviewBranchRef {
                name: "main".into(),
            },
            head: GithubReviewHeadRef {
                name: "fork/review".into(),
                sha: "def456".into(),
            },
        },
    ]);

    assert_eq!(events.len(), 2);
    assert_eq!(events[0].number, 42);
    assert_eq!(events[0].head_sha, "abc123");
    assert_eq!(events[0].author_login.as_deref(), Some("alice"));
    assert_eq!(events[0].approval_state, DispatchApprovalState::Approved);
    assert_eq!(events[0].labels, vec!["needs review"]);
    assert_eq!(events[0].checkout_ref.as_deref(), Some("refs/pull/42/head"));
    assert_eq!(events[1].number, 43);
    assert_eq!(events[1].author_login.as_deref(), Some("dependabot[bot]"));
    assert_eq!(events[1].approval_state, DispatchApprovalState::Approved);
    assert_eq!(events[1].checkout_ref.as_deref(), Some("refs/pull/43/head"));
}

#[test]
fn conflict_event_detection_uses_mergeable_and_merge_state_status() {
    assert!(should_emit_conflict_event(
        &fetch_pull_request_events::MergeableState::CONFLICTING,
        &fetch_pull_request_events::MergeStateStatus::CLEAN,
    ));
    assert!(should_emit_conflict_event(
        &fetch_pull_request_events::MergeableState::MERGEABLE,
        &fetch_pull_request_events::MergeStateStatus::DIRTY,
    ));
    assert!(!should_emit_conflict_event(
        &fetch_pull_request_events::MergeableState::MERGEABLE,
        &fetch_pull_request_events::MergeStateStatus::CLEAN,
    ));
}

#[test]
fn find_issue_comment_id_with_marker_matches_existing_review_comment() {
    let comments = vec![
        GithubIssueCommentResponse {
            id: 1,
            body: Some("hello".into()),
        },
        GithubIssueCommentResponse {
            id: 2,
            body: Some(
                "review\n\n<!-- polyphony:pr-review github penso/polyphony#42 sha=abc123 -->"
                    .into(),
            ),
        },
    ];

    assert_eq!(
        find_issue_comment_id_with_marker(
            &comments,
            "<!-- polyphony:pr-review github penso/polyphony#42 sha=abc123 -->",
        ),
        Some(2)
    );
}

#[test]
fn github_issue_approval_waits_for_outsiders_and_approves_collaborators() {
    assert_eq!(
        github_issue_approval_state(Some(&AuthorAssociation::Owner), Some("repo-owner")),
        DispatchApprovalState::Approved
    );
    assert_eq!(
        github_issue_approval_state(Some(&AuthorAssociation::Collaborator), Some("teammate"),),
        DispatchApprovalState::Approved
    );
    assert_eq!(
        github_issue_approval_state(
            Some(&AuthorAssociation::Contributor),
            Some("dependabot[bot]"),
        ),
        DispatchApprovalState::Approved
    );
    assert_eq!(
        github_issue_approval_state(Some(&AuthorAssociation::Contributor), Some("outsider")),
        DispatchApprovalState::Waiting
    );
    assert_eq!(
        github_issue_approval_state(Some(&AuthorAssociation::FirstTimer), Some("newcomer")),
        DispatchApprovalState::Waiting
    );
}
