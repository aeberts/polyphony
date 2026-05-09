use std::{collections::BTreeMap, path::Path};

use chrono::{DateTime, Utc};
use minijinja::Environment;
use polyphony_core::RuntimeSnapshot;
use serde::Serialize;
use serde_json::{Map, Value};

pub(crate) fn build_env(template_dir: &Path) -> Environment<'static> {
    let mut env = Environment::new();
    env.set_loader(minijinja::path_loader(template_dir));
    env
}

pub(crate) fn snapshot_context_object(snapshot: &RuntimeSnapshot) -> Map<String, Value> {
    let mut context = match serde_json::to_value(snapshot) {
        Ok(Value::Object(object)) => object,
        Ok(_) | Err(_) => Map::new(),
    };
    if let Ok(provider_budgets) = serde_json::to_value(provider_budget_summaries(snapshot)) {
        context.insert("provider_budgets".into(), provider_budgets);
    }
    if let Ok(run_insights) = serde_json::to_value(run_insight_map(snapshot)) {
        context.insert("run_insights".into(), run_insights);
    }
    if let Ok(heartbeat_events) = serde_json::to_value(heartbeat_events(snapshot)) {
        context.insert("heartbeat_events".into(), heartbeat_events);
    }
    context
}

pub(crate) fn snapshot_context(snapshot: &RuntimeSnapshot) -> minijinja::Value {
    minijinja::Value::from_serialize(snapshot_context_object(snapshot))
}

#[derive(Debug, Clone, Serialize, PartialEq)]
struct ProviderBudgetSummary {
    provider: String,
    component: String,
    captured_at: DateTime<Utc>,
    throttled: bool,
    session_remaining_percent: Option<f64>,
    session_label: String,
    session_reset_at: Option<DateTime<Utc>>,
    weekly_remaining_percent: Option<f64>,
    weekly_label: String,
    weekly_deficit_percent: f64,
    weekly_reserve_percent: f64,
    weekly_pace_label: String,
    weekly_eta_seconds: Option<i64>,
    weekly_reset_at: Option<DateTime<Utc>>,
    eta_label: String,
}

fn provider_budget_summaries(snapshot: &RuntimeSnapshot) -> Vec<ProviderBudgetSummary> {
    let mut providers = BTreeMap::new();
    for budget in &snapshot.budgets {
        let provider = budget
            .raw
            .as_ref()
            .and_then(|raw| raw.get("provider").and_then(Value::as_str))
            .map(str::to_owned)
            .unwrap_or_else(|| {
                budget
                    .component
                    .strip_prefix("agent:")
                    .unwrap_or(&budget.component)
                    .to_string()
            });
        let summary = provider_budget_summary(&provider, budget);
        providers
            .entry(provider)
            .and_modify(|existing: &mut ProviderBudgetSummary| {
                if summary.captured_at > existing.captured_at {
                    *existing = summary.clone();
                }
            })
            .or_insert(summary);
    }
    for throttle in &snapshot.throttles {
        let Some(provider) = throttle.component.strip_prefix("budget:") else {
            continue;
        };
        providers.entry(provider.to_string()).or_insert_with(|| {
            throttled_provider_summary(provider, throttle, snapshot.generated_at)
        });
    }
    let mut values: Vec<_> = providers.into_values().collect();
    values.sort_by(|left, right| {
        provider_rank(left.provider.as_str())
            .cmp(&provider_rank(right.provider.as_str()))
            .then_with(|| left.provider.cmp(&right.provider))
    });
    values
}

fn provider_budget_summary(
    provider: &str,
    budget: &polyphony_core::BudgetSnapshot,
) -> ProviderBudgetSummary {
    let raw = budget.raw.as_ref();
    let session_remaining_percent = raw
        .and_then(|value| value.pointer("/session/remaining_percent"))
        .and_then(Value::as_f64)
        .or_else(|| {
            budget
                .credits_remaining
                .zip(budget.credits_total)
                .map(|(remaining, total)| {
                    if total > 0.0 {
                        (remaining / total) * 100.0
                    } else {
                        remaining
                    }
                })
        })
        .or(budget.credits_remaining);
    let session_reset_at = raw
        .and_then(|value| value.pointer("/session/reset_at"))
        .and_then(Value::as_str)
        .and_then(parse_rfc3339)
        .or(budget.reset_at);
    let weekly_remaining_percent = raw
        .and_then(|value| value.pointer("/weekly/remaining_percent"))
        .and_then(Value::as_f64);
    let weekly_deficit_percent = raw
        .and_then(|value| value.pointer("/weekly/deficit_percent"))
        .and_then(Value::as_f64)
        .unwrap_or_default();
    let weekly_reserve_percent = raw
        .and_then(|value| value.pointer("/weekly/reserve_percent"))
        .and_then(Value::as_f64)
        .unwrap_or_default();
    let weekly_eta_seconds = raw
        .and_then(|value| value.pointer("/weekly/eta_seconds"))
        .and_then(Value::as_i64);
    let weekly_reset_at = raw
        .and_then(|value| value.pointer("/weekly/reset_at"))
        .and_then(Value::as_str)
        .and_then(parse_rfc3339);
    ProviderBudgetSummary {
        provider: provider.to_string(),
        component: budget.component.clone(),
        captured_at: budget.captured_at,
        throttled: false,
        session_remaining_percent,
        session_label: percent_label(session_remaining_percent),
        session_reset_at,
        weekly_remaining_percent,
        weekly_label: percent_label(weekly_remaining_percent),
        weekly_deficit_percent,
        weekly_reserve_percent,
        weekly_pace_label: weekly_pace_label(weekly_deficit_percent, weekly_reserve_percent),
        weekly_eta_seconds,
        weekly_reset_at,
        eta_label: weekly_eta_seconds
            .map(short_eta_label)
            .or_else(|| weekly_reset_at.map(short_reset_label))
            .unwrap_or_else(|| "n/a".into()),
    }
}

fn throttled_provider_summary(
    provider: &str,
    throttle: &polyphony_core::ThrottleWindow,
    captured_at: DateTime<Utc>,
) -> ProviderBudgetSummary {
    ProviderBudgetSummary {
        provider: provider.to_string(),
        component: throttle.component.clone(),
        captured_at,
        throttled: true,
        session_remaining_percent: None,
        session_label: "throttled".into(),
        session_reset_at: Some(throttle.until),
        weekly_remaining_percent: None,
        weekly_label: "throttled".into(),
        weekly_deficit_percent: 0.0,
        weekly_reserve_percent: 0.0,
        weekly_pace_label: "throttled".into(),
        weekly_eta_seconds: Some(
            throttle
                .until
                .signed_duration_since(Utc::now())
                .num_seconds(),
        ),
        weekly_reset_at: Some(throttle.until),
        eta_label: short_reset_label(throttle.until),
    }
}

fn percent_label(value: Option<f64>) -> String {
    value
        .map(|value| format!("{value:.0}%"))
        .unwrap_or_else(|| "n/a".into())
}

fn weekly_pace_label(deficit_percent: f64, reserve_percent: f64) -> String {
    if deficit_percent > 0.0 {
        format!("Δ{deficit_percent:.0}%")
    } else if reserve_percent > 0.0 {
        format!("R{reserve_percent:.0}%")
    } else {
        "flat".into()
    }
}

fn parse_rfc3339(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|value| value.with_timezone(&Utc))
}

fn short_reset_label(reset_at: DateTime<Utc>) -> String {
    short_eta_label(reset_at.signed_duration_since(Utc::now()).num_seconds())
}

fn short_eta_label(seconds: i64) -> String {
    let seconds = seconds.max(0);
    let days = seconds / 86_400;
    let hours = (seconds % 86_400) / 3_600;
    let minutes = (seconds % 3_600) / 60;
    if days > 0 {
        format!("{days}d{hours}h")
    } else if hours > 0 {
        format!("{hours}h{minutes:02}m")
    } else {
        format!("{minutes}m")
    }
}

fn provider_rank(provider: &str) -> u8 {
    match provider {
        "codex" => 0,
        "claude" => 1,
        _ => 2,
    }
}

fn run_insight_map(snapshot: &RuntimeSnapshot) -> BTreeMap<String, polyphony_core::RunInsight> {
    snapshot
        .runs
        .iter()
        .map(|run| {
            let tasks = snapshot
                .tasks
                .iter()
                .filter(|task| task.run_id == run.id)
                .cloned()
                .collect::<Vec<_>>();
            let history = snapshot
                .agent_run_history
                .iter()
                .filter(|entry| polyphony_core::agent_history_matches_run(run, entry))
                .cloned()
                .collect::<Vec<_>>();
            let running = snapshot
                .running
                .iter()
                .filter(|entry| polyphony_core::running_agent_matches_run(run, entry))
                .cloned()
                .collect::<Vec<_>>();
            (
                run.id.clone(),
                polyphony_core::build_run_insight(run, &tasks, &history, &running),
            )
        })
        .collect()
}

fn heartbeat_events(snapshot: &RuntimeSnapshot) -> Vec<polyphony_core::RuntimeEvent> {
    snapshot
        .recent_events
        .iter()
        .filter(|event| event.scope == polyphony_core::EventScope::Heartbeat)
        .cloned()
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn embedded_templates_parse() {
        let template_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("templates");
        let env = build_env(&template_dir);
        for name in [
            "index.html",
            "runs.html",
            "inbox.html",
            "agents.html",
            "outcomes.html",
            "tasks.html",
            "repos.html",
            "logs.html",
            "heartbeat.html",
            "docs.html",
            "layout.html",
            "login.html",
            "users.html",
        ] {
            env.get_template(name)
                .unwrap_or_else(|e| panic!("template {name} failed to parse: {e}"));
        }
    }

    #[test]
    fn snapshot_context_includes_provider_budgets() {
        let snapshot: RuntimeSnapshot = serde_json::from_value(json!({
            "generated_at": "2026-01-01T00:00:00Z",
            "counts": { "running": 0, "retrying": 0, "runs": 0, "tasks_pending": 0, "tasks_in_progress": 0, "tasks_completed": 0, "worktrees": 0 },
            "running": [],
            "retrying": [],
            "codex_totals": { "input_tokens": 0, "output_tokens": 0, "total_tokens": 0, "seconds_running": 0.0 },
            "rate_limits": null,
            "throttles": [
                {
                    "component": "budget:claude",
                    "until": "2026-01-01T01:00:00Z",
                    "reason": "weekly limit"
                }
            ],
            "budgets": [
                {
                    "component": "agent:codex-router",
                    "captured_at": "2026-01-01T00:00:00Z",
                    "credits_remaining": 80.0,
                    "credits_total": 100.0,
                    "spent_usd": null,
                    "soft_limit_usd": null,
                    "hard_limit_usd": null,
                    "reset_at": "2026-01-02T00:00:00Z",
                    "raw": {
                        "provider": "codex",
                        "session": { "remaining_percent": 80.0, "reset_at": "2026-01-02T00:00:00Z" },
                        "weekly": { "remaining_percent": 60.0, "reserve_percent": 12.0, "reset_at": "2026-01-08T00:00:00Z" }
                    }
                }
            ],
            "agent_catalogs": [],
            "saved_contexts": [],
            "recent_events": []
        }))
        .expect("snapshot should deserialize");

        let context = snapshot_context(&snapshot);
        let serialized = serde_json::to_value(&context).expect("context should serialize");
        let provider_budgets = serialized["provider_budgets"]
            .as_array()
            .expect("provider budgets should be an array");
        assert_eq!(provider_budgets.len(), 2);
        assert_eq!(provider_budgets[0]["provider"], "codex");
        assert_eq!(provider_budgets[0]["session_remaining_percent"], 80.0);
        assert_eq!(provider_budgets[0]["weekly_remaining_percent"], 60.0);
        assert_eq!(provider_budgets[1]["provider"], "claude");
        assert_eq!(provider_budgets[1]["throttled"], true);
        assert_eq!(provider_budgets[1]["session_label"], "throttled");
    }

    #[test]
    fn snapshot_context_includes_heartbeat_events() {
        let snapshot: RuntimeSnapshot = serde_json::from_value(json!({
            "generated_at": "2026-01-01T00:00:00Z",
            "counts": { "running": 0, "retrying": 0, "runs": 0, "tasks_pending": 0, "tasks_in_progress": 0, "tasks_completed": 0, "worktrees": 0 },
            "running": [],
            "retrying": [],
            "codex_totals": { "input_tokens": 0, "output_tokens": 0, "total_tokens": 0, "seconds_running": 0.0 },
            "rate_limits": null,
            "throttles": [],
            "budgets": [],
            "agent_catalogs": [],
            "saved_contexts": [],
            "recent_events": [
                {
                    "scope": "dispatch",
                    "message": "rule-based dispatch",
                    "at": "2026-01-01T00:00:00Z"
                },
                {
                    "scope": "heartbeat",
                    "message": "heartbeat dispatched GH-1",
                    "at": "2026-01-01T00:01:00Z"
                }
            ]
        }))
        .expect("snapshot should deserialize");

        let context = snapshot_context_object(&snapshot);
        let events = context
            .get("heartbeat_events")
            .and_then(Value::as_array)
            .cloned()
            .expect("heartbeat_events should be present");

        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].get("scope").and_then(Value::as_str),
            Some("heartbeat")
        );
    }

    #[test]
    fn snapshot_context_contains_run_insights() {
        let snapshot: RuntimeSnapshot = serde_json::from_value(json!({
            "repo_ids": [],
            "repo_registrations": [],
            "generated_at": "2025-01-01T00:00:00Z",
            "counts": {
              "running": 0, "retrying": 0, "worktrees": 0, "runs": 1,
              "tasks_pending": 0, "tasks_in_progress": 0, "tasks_completed": 1
            },
            "cadence": {
              "tracker_poll_interval_ms": 0,
              "budget_poll_interval_ms": 0,
              "model_discovery_interval_ms": 0,
              "last_tracker_poll_at": null,
              "last_budget_poll_at": null,
              "last_model_discovery_at": null
            },
            "tracker_issues": [],
            "inbox_items": [],
            "approved_inbox_keys": [],
            "running": [],
            "agent_run_history": [{
              "repo_id": "penso/polyphony",
              "run_id": "run-1",
              "issue_id": "issue-1",
              "issue_identifier": "GH-1",
              "agent_name": "codex",
              "model": "gpt-5",
              "status": "Succeeded",
              "attempt": 1,
              "max_turns": 4,
              "turn_count": 2,
              "session_id": "sess-1",
              "thread_id": null,
              "turn_id": null,
              "codex_app_server_pid": null,
              "last_event": "completed",
              "last_message": "done",
              "started_at": "2024-12-31T23:58:00Z",
              "finished_at": "2024-12-31T23:59:30Z",
              "last_event_at": "2024-12-31T23:59:30Z",
              "tokens": { "input_tokens": 20, "output_tokens": 22, "total_tokens": 42 },
              "workspace_path": "/tmp/workspace",
              "error": null,
              "saved_context": null
            }],
            "retrying": [],
            "codex_totals": { "input_tokens": 0, "output_tokens": 0, "total_tokens": 0, "seconds_running": 0.0 },
            "rate_limits": null,
            "throttles": [],
            "budgets": [],
            "agent_catalogs": [],
            "saved_contexts": [],
            "recent_events": [],
            "pending_user_interactions": [],
            "runs": [{
              "repo_id": "penso/polyphony",
              "id": "run-1",
              "kind": "issue_delivery",
              "issue_identifier": "GH-1",
              "title": "Ship the thing",
              "status": "delivered",
              "task_count": 1,
              "tasks_completed": 1,
              "has_deliverable": true,
              "deliverable": {
                "kind": "local_branch",
                "status": "open",
                "url": null,
                "decision": "waiting",
                "title": "Branch: feat/ship",
                "description": null,
                "metadata": { "branch": "feat/ship", "changed_files": 2 }
              },
              "review_target": null,
              "workspace_key": "gh-1",
              "workspace_path": "/tmp/workspace",
              "created_at": "2025-01-01T00:00:00Z",
              "activity_log": [],
              "cancel_reason": null,
              "steps": []
            }],
            "tasks": [{
              "repo_id": "penso/polyphony",
              "id": "task-1",
              "run_id": "run-1",
              "title": "Implement",
              "description": null,
              "activity_log": [],
              "category": "coding",
              "status": "completed",
              "ordinal": 1,
              "agent_name": "codex",
              "turns_completed": 1,
              "total_tokens": 42,
              "started_at": null,
              "finished_at": null,
              "error": null,
              "created_at": "2025-01-01T00:00:00Z",
              "updated_at": "2025-01-01T00:00:00Z"
            }],
            "loading": {
              "fetching_issues": false,
              "fetching_budgets": false,
              "fetching_models": false,
              "reconciling": false
            },
            "dispatch_mode": "manual",
            "tracker_kind": "none",
            "tracker_connection": null,
            "from_cache": false,
            "cached_at": null,
            "agent_profile_names": [],
            "agent_profiles": [],
            "heartbeat": {
              "enabled": false,
              "agent_name": null,
              "last_run_at": null,
              "last_decision": null,
              "total_tokens_used": 0,
              "fallback_count": 0
            }
        }))
        .unwrap();

        let context = snapshot_context_object(&snapshot);
        let insight = context
            .get("run_insights")
            .and_then(|value| value.get("run-1"))
            .cloned()
            .unwrap();

        assert_eq!(
            insight.get("summary").and_then(Value::as_str),
            Some("Delivered as `feat/ship` with 2 changed files.")
        );
        assert_eq!(
            insight
                .get("history_facts")
                .and_then(Value::as_array)
                .and_then(|facts| facts.first())
                .and_then(|fact| fact.get("label"))
                .and_then(Value::as_str),
            Some("Attempts")
        );
    }

    #[test]
    fn inbox_template_hides_duplicate_child_repo_and_updated_cells() {
        let template_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("templates");
        let env = build_env(&template_dir);
        let template = env
            .get_template("inbox.html")
            .expect("inbox template should load");
        let snapshot: RuntimeSnapshot = serde_json::from_value(json!({
            "repo_ids": ["polyphony"],
            "repo_registrations": [],
            "generated_at": "2026-01-01T00:00:00Z",
            "counts": {
              "running": 0, "retrying": 0, "worktrees": 0, "runs": 0,
              "tasks_pending": 0, "tasks_in_progress": 0, "tasks_completed": 0
            },
            "cadence": {
              "tracker_poll_interval_ms": 0,
              "budget_poll_interval_ms": 0,
              "model_discovery_interval_ms": 0,
              "last_tracker_poll_at": null,
              "last_budget_poll_at": null,
              "last_model_discovery_at": null
            },
            "tracker_issues": [],
            "inbox_items": [
              {
                "repo_id": "polyphony",
                "item_id": "parent",
                "kind": "issue",
                "source": "beads",
                "identifier": "oio",
                "title": "Parent issue",
                "status": "Open",
                "approval_state": "approved",
                "priority": 2,
                "labels": [],
                "description": null,
                "url": null,
                "author": null,
                "parent_id": null,
                "updated_at": "2026-03-12T23:39:43Z",
                "created_at": "2026-03-12T23:39:43Z",
                "has_workspace": false
              },
              {
                "repo_id": "polyphony",
                "item_id": "child",
                "kind": "issue",
                "source": "beads",
                "identifier": "oio.4",
                "title": "Child issue",
                "status": "Open",
                "approval_state": "approved",
                "priority": 2,
                "labels": [],
                "description": null,
                "url": null,
                "author": null,
                "parent_id": "parent",
                "updated_at": "2026-03-12T23:39:44Z",
                "created_at": "2026-03-12T23:39:44Z",
                "has_workspace": false
              }
            ],
            "approved_inbox_keys": [],
            "running": [],
            "agent_run_history": [],
            "retrying": [],
            "codex_totals": { "input_tokens": 0, "output_tokens": 0, "total_tokens": 0, "seconds_running": 0.0 },
            "rate_limits": null,
            "throttles": [],
            "budgets": [],
            "agent_catalogs": [],
            "saved_contexts": [],
            "recent_events": [],
            "pending_user_interactions": [],
            "runs": [],
            "tasks": [],
            "loading": {
              "fetching_issues": false,
              "fetching_budgets": false,
              "fetching_models": false,
              "reconciling": false
            },
            "dispatch_mode": "manual",
            "tracker_kind": "none",
            "tracker_connection": null,
            "from_cache": false,
            "cached_at": null,
            "agent_profile_names": [],
            "agent_profiles": [],
            "heartbeat": {
              "enabled": false,
              "agent_name": null,
              "last_run_at": null,
              "last_decision": null,
              "total_tokens_used": 0,
              "fallback_count": 0
            }
        }))
        .expect("snapshot should deserialize");

        let rendered = template
            .render(snapshot_context(&snapshot))
            .expect("inbox template should render");

        let child_start = rendered
            .find("data-item-id=\"child\"")
            .expect("child row should be present");
        let child_section = &rendered[child_start..rendered.len().min(child_start + 800)];

        assert!(child_section.contains("Child issue"));
        assert!(child_section.contains("&nbsp;"));
        assert!(!child_section.contains("<time datetime=\"2026-03-12T23:39:44Z\">"));
        assert!(!child_section.contains(">polyphony</span>"));
    }

    #[test]
    fn agents_template_hides_issue_identifier_column() {
        let template_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("templates");
        let env = build_env(&template_dir);
        let template = env
            .get_template("agents.html")
            .expect("agents template should load");
        let snapshot: RuntimeSnapshot = serde_json::from_value(json!({
            "repo_ids": ["polyphony"],
            "repo_registrations": [],
            "generated_at": "2026-01-01T00:00:00Z",
            "counts": {
              "running": 1, "retrying": 0, "worktrees": 0, "runs": 0,
              "tasks_pending": 0, "tasks_in_progress": 0, "tasks_completed": 0
            },
            "cadence": {
              "tracker_poll_interval_ms": 0,
              "budget_poll_interval_ms": 0,
              "model_discovery_interval_ms": 0,
              "last_tracker_poll_at": null,
              "last_budget_poll_at": null,
              "last_model_discovery_at": null
            },
            "tracker_issues": [],
            "inbox_items": [],
            "approved_inbox_keys": [],
            "running": [
              {
                "repo_id": "polyphony",
                "run_id": "run-1",
                "issue_id": "issue-1",
                "issue_identifier": "polyphony-123",
                "agent_name": "codex",
                "model": "gpt-5.4",
                "state": "running",
                "max_turns": 12,
                "session_id": null,
                "thread_id": null,
                "turn_id": null,
                "codex_app_server_pid": null,
                "turn_count": 3,
                "last_event": null,
                "last_message": null,
                "started_at": "2026-03-12T23:39:43Z",
                "last_event_at": null,
                "tokens": {
                  "input_tokens": 0,
                  "output_tokens": 0,
                  "total_tokens": 0
                },
                "workspace_path": "/tmp/polyphony",
                "attempt": null,
                "recent_log": []
              }
            ],
            "agent_run_history": [
              {
                "repo_id": "polyphony",
                "run_id": "run-1",
                "issue_id": "issue-1",
                "issue_identifier": "polyphony-123",
                "agent_name": "codex",
                "model": "gpt-5.4",
                "status": "Succeeded",
                "max_turns": 12,
                "turn_count": 3,
                "tokens": {
                  "input_tokens": 0,
                  "output_tokens": 0,
                  "total_tokens": 0
                },
                "started_at": "2026-03-12T23:39:43Z",
                "finished_at": "2026-03-12T23:49:43Z",
                "attempt": 1,
                "error": null,
                "last_event": null,
                "last_message": null,
                "workspace_path": "/tmp/polyphony",
                "session_id": null,
                "saved_context": null
              }
            ],
            "retrying": [],
            "codex_totals": { "input_tokens": 0, "output_tokens": 0, "total_tokens": 0, "seconds_running": 0.0 },
            "rate_limits": null,
            "throttles": [],
            "budgets": [],
            "agent_catalogs": [],
            "saved_contexts": [],
            "recent_events": [],
            "pending_user_interactions": [],
            "runs": [],
            "tasks": [],
            "loading": {
              "fetching_issues": false,
              "fetching_budgets": false,
              "fetching_models": false,
              "reconciling": false
            },
            "dispatch_mode": "manual",
            "tracker_kind": "none",
            "tracker_connection": null,
            "from_cache": false,
            "cached_at": null,
            "agent_profile_names": [],
            "agent_profiles": [],
            "heartbeat": {
              "enabled": false,
              "agent_name": null,
              "last_run_at": null,
              "last_decision": null,
              "total_tokens_used": 0,
              "fallback_count": 0
            }
        }))
        .expect("snapshot should deserialize");

        let rendered = template
            .render(snapshot_context(&snapshot))
            .expect("agents template should render");

        assert!(rendered.contains("<span>Started</span>"));
        assert!(rendered.contains("<span>Finished</span>"));
        assert!(!rendered.contains("<span>Issue</span>"));
        assert!(!rendered.contains("<span class=\"meta-label\">issue:</span>"));
        assert!(rendered.contains("<h2>codex</h2>"));
    }

    #[test]
    fn tasks_template_hides_run_column_and_uses_icon_status_with_time_first() {
        let template_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("templates");
        let env = build_env(&template_dir);
        let template = env
            .get_template("tasks.html")
            .expect("tasks template should load");
        let snapshot: RuntimeSnapshot = serde_json::from_value(json!({
            "repo_ids": ["polyphony"],
            "repo_registrations": [],
            "generated_at": "2026-01-01T00:00:00Z",
            "counts": {
              "running": 0, "retrying": 0, "worktrees": 0, "runs": 0,
              "tasks_pending": 1, "tasks_in_progress": 0, "tasks_completed": 0
            },
            "cadence": {
              "tracker_poll_interval_ms": 0,
              "budget_poll_interval_ms": 0,
              "model_discovery_interval_ms": 0,
              "last_tracker_poll_at": null,
              "last_budget_poll_at": null,
              "last_model_discovery_at": null
            },
            "tracker_issues": [],
            "inbox_items": [],
            "approved_inbox_keys": [],
            "running": [],
            "agent_run_history": [],
            "retrying": [],
            "codex_totals": { "input_tokens": 0, "output_tokens": 0, "total_tokens": 0, "seconds_running": 0.0 },
            "rate_limits": null,
            "throttles": [],
            "budgets": [],
            "agent_catalogs": [],
            "saved_contexts": [],
            "recent_events": [],
            "pending_user_interactions": [],
            "runs": [],
            "tasks": [
              {
                "repo_id": "polyphony",
                "id": "task-1",
                "run_id": "run-1",
                "title": "Implement task table cleanup",
                "description": null,
                "activity_log": [],
                "category": "coding",
                "status": "pending",
                "ordinal": 1,
                "agent_name": "codex",
                "turns_completed": 0,
                "total_tokens": 0,
                "started_at": null,
                "finished_at": null,
                "error": null,
                "created_at": "2026-03-12T23:39:43Z",
                "updated_at": "2026-03-12T23:39:44Z"
              }
            ],
            "loading": {
              "fetching_issues": false,
              "fetching_budgets": false,
              "fetching_models": false,
              "reconciling": false
            },
            "dispatch_mode": "manual",
            "tracker_kind": "none",
            "tracker_connection": null,
            "from_cache": false,
            "cached_at": null,
            "agent_profile_names": [],
            "agent_profiles": [],
            "heartbeat": {
              "enabled": false,
              "agent_name": null,
              "last_run_at": null,
              "last_decision": null,
              "total_tokens_used": 0,
              "fallback_count": 0
            }
        }))
        .expect("snapshot should deserialize");

        let rendered = template
            .render(snapshot_context(&snapshot))
            .expect("tasks template should render");

        let updated_pos = rendered
            .find("<span>Updated</span>")
            .expect("updated header");
        let repo_pos = rendered.find("<span>Repo</span>").expect("repo header");
        assert!(
            updated_pos < repo_pos,
            "time should be first data column: {rendered}"
        );
        assert!(!rendered.contains("<span>Run</span>"));
        assert!(rendered.contains("&#x25F7;</span> pending"));

        let task_start = rendered
            .find("data-task-id=\"task-1\"")
            .expect("task row should exist");
        let task_section = &rendered[task_start..rendered.len().min(task_start + 500)];
        assert!(task_section.contains("<time datetime=\"2026-03-12T23:39:44Z\">"));
        assert!(!task_section.contains(">pending</span>"));
    }

    #[test]
    fn repos_template_shows_added_column_before_repository() {
        let template_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("templates");
        let env = build_env(&template_dir);
        let template = env
            .get_template("repos.html")
            .expect("repos template should load");
        let snapshot: RuntimeSnapshot = serde_json::from_value(json!({
            "repo_ids": ["polyphony"],
            "repo_registrations": [
              {
                "repo_id": "polyphony",
                "label": "polyphony",
                "worktree_path": "/tmp/polyphony",
                "clone_url": "https://github.com/penso/polyphony",
                "default_branch": "main",
                "tracker_kind": "github",
                "added_at": "2026-03-12T23:39:44Z"
              }
            ],
            "generated_at": "2026-01-01T00:00:00Z",
            "counts": {
              "running": 0, "retrying": 0, "worktrees": 0, "runs": 0,
              "tasks_pending": 0, "tasks_in_progress": 0, "tasks_completed": 0
            },
            "cadence": {
              "tracker_poll_interval_ms": 0,
              "budget_poll_interval_ms": 0,
              "model_discovery_interval_ms": 0,
              "last_tracker_poll_at": null,
              "last_budget_poll_at": null,
              "last_model_discovery_at": null
            },
            "tracker_issues": [],
            "inbox_items": [],
            "approved_inbox_keys": [],
            "running": [],
            "agent_run_history": [],
            "retrying": [],
            "codex_totals": { "input_tokens": 0, "output_tokens": 0, "total_tokens": 0, "seconds_running": 0.0 },
            "rate_limits": null,
            "throttles": [],
            "budgets": [],
            "agent_catalogs": [],
            "saved_contexts": [],
            "recent_events": [],
            "pending_user_interactions": [],
            "runs": [],
            "tasks": [],
            "loading": {
              "fetching_issues": false,
              "fetching_budgets": false,
              "fetching_models": false,
              "reconciling": false
            },
            "dispatch_mode": "manual",
            "tracker_kind": "none",
            "tracker_connection": null,
            "from_cache": false,
            "cached_at": null,
            "agent_profile_names": [],
            "agent_profiles": [],
            "heartbeat": {
              "enabled": false,
              "agent_name": null,
              "last_run_at": null,
              "last_decision": null,
              "total_tokens_used": 0,
              "fallback_count": 0
            }
        }))
        .expect("snapshot should deserialize");

        let rendered = template
            .render(snapshot_context(&snapshot))
            .expect("repos template should render");

        let added_pos = rendered.find("<span>Added</span>").expect("added header");
        let repo_pos = rendered
            .find("<span>Repository</span>")
            .expect("repository header");
        assert!(
            added_pos < repo_pos,
            "added should be first data column: {rendered}"
        );
    }
}
