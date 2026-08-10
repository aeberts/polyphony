use serde::{Deserialize, Serialize};

use crate::{
    AgentRunHistoryRow, DeliverableKind, DeliverableStatus, RunRow, RunStatus, RunningAgentRow,
    StepKind, TaskRow, TaskStatus,
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunArtifactFact {
    pub label: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunInsight {
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_action: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_activity: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub history_facts: Vec<RunArtifactFact>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_facts: Vec<RunArtifactFact>,
}

pub fn build_run_insight(
    run: &RunRow,
    tasks: &[TaskRow],
    history: &[AgentRunHistoryRow],
    running: &[RunningAgentRow],
) -> RunInsight {
    let stop_reason = run
        .cancel_reason
        .clone()
        .or_else(|| {
            run.steps
                .iter()
                .find(|step| step.status == crate::StepStatus::Failed)
                .and_then(|step| step.error.clone())
        })
        .or_else(|| {
            tasks
                .iter()
                .find(|task| task.status == TaskStatus::Failed)
                .and_then(|task| task.error.clone())
        });
    let last_activity = run.activity_log.last().map(|entry| entry.message.clone());

    let in_progress_titles = tasks
        .iter()
        .filter(|task| task.status == TaskStatus::InProgress)
        .map(|task| task.title.as_str())
        .collect::<Vec<_>>();
    let failed_task = tasks.iter().find(|task| task.status == TaskStatus::Failed);
    let failed_step = run
        .steps
        .iter()
        .find(|step| step.status == crate::StepStatus::Failed);

    let summary = match run.status {
        RunStatus::Pending => "Queued and waiting to start.".into(),
        RunStatus::Planning => {
            if in_progress_titles.is_empty() {
                "Planning work before execution starts.".into()
            } else {
                format!(
                    "Planning work, active now: {}.",
                    in_progress_titles.join(", ")
                )
            }
        },
        RunStatus::InProgress => {
            let base = if run.task_count > 0 {
                format!(
                    "In progress, completed {} of {} tasks.",
                    run.tasks_completed, run.task_count
                )
            } else {
                "In progress.".into()
            };
            if in_progress_titles.is_empty() {
                base
            } else {
                format!("{base} Active now: {}.", in_progress_titles.join(", "))
            }
        },
        RunStatus::Review => "Execution finished, waiting in review.".into(),
        RunStatus::Delivered => delivered_summary(run),
        RunStatus::Failed => {
            if let Some(step) = failed_step {
                format!("Failed during {}.", step_kind_label(step.kind))
            } else if let Some(task) = failed_task {
                format!("Failed while executing task {}.", quoted(&task.title))
            } else {
                "Run failed.".into()
            }
        },
        RunStatus::Cancelled => stop_reason
            .as_ref()
            .map(|reason| format!("Cancelled. {reason}"))
            .unwrap_or_else(|| "Run cancelled.".into()),
        RunStatus::Blocked => run
            .blocked_outcome
            .as_ref()
            .map(|outcome| {
                format!(
                    "Blocked pending {}: {}",
                    outcome.prerequisite, outcome.reason
                )
            })
            .unwrap_or_else(|| "Run is blocked pending prerequisite work.".into()),
    };

    let next_action = next_action(run);

    RunInsight {
        summary,
        next_action,
        stop_reason,
        last_activity,
        history_facts: history_facts(tasks, history, running),
        artifact_facts: artifact_facts(run),
    }
}

pub fn agent_history_matches_run(run: &RunRow, history: &AgentRunHistoryRow) -> bool {
    history.run_id.as_deref() == Some(run.id.as_str())
        || history.run_id.is_none()
            && run.issue_identifier.as_deref() == Some(history.issue_identifier.as_str())
}

pub fn running_agent_matches_run(run: &RunRow, running: &RunningAgentRow) -> bool {
    running.run_id.as_deref() == Some(run.id.as_str())
        || running.run_id.is_none()
            && run.issue_identifier.as_deref() == Some(running.issue_identifier.as_str())
}

fn delivered_summary(run: &RunRow) -> String {
    let Some(deliverable) = &run.deliverable else {
        return "Delivered.".into();
    };
    let kind = deliverable_kind_label(deliverable.kind);
    let changed_files = metadata_u64(run, "changed_files");
    match deliverable.kind {
        DeliverableKind::PullRequestReview => {
            let verdict = metadata_str(run, "verdict").unwrap_or_else(|| "review".into());
            format!(
                "Delivered as a pull request review with {} verdict.",
                verdict
            )
        },
        DeliverableKind::LocalBranch => {
            let branch = metadata_str(run, "branch").unwrap_or_else(|| "local branch".into());
            if let Some(changed_files) = changed_files {
                format!(
                    "Delivered as {} with {} changed file{}.",
                    quoted(&branch),
                    changed_files,
                    plural(changed_files)
                )
            } else {
                format!("Delivered as {}.", quoted(&branch))
            }
        },
        _ => {
            if let Some(changed_files) = changed_files {
                format!(
                    "Delivered as {kind} with {} changed file{}.",
                    changed_files,
                    plural(changed_files)
                )
            } else {
                format!("Delivered as {kind}.")
            }
        },
    }
}

fn next_action(run: &RunRow) -> Option<String> {
    let deliverable = run.deliverable.as_ref();
    if let Some(deliverable) = deliverable
        && deliverable.decision == crate::DeliverableDecision::Waiting
    {
        return Some("Review the deliverable, then accept or reject it.".into());
    }
    if let Some(deliverable) = deliverable
        && deliverable.status == DeliverableStatus::Open
        && deliverable.decision == crate::DeliverableDecision::Accepted
    {
        return Some("Merge the deliverable when it is ready.".into());
    }
    match run.status {
        RunStatus::Failed => Some("Inspect the failure, then retry the run.".into()),
        RunStatus::Review => Some("Inspect the review output and decide on handoff.".into()),
        RunStatus::Cancelled => Some("Retry after addressing the cancellation reason.".into()),
        RunStatus::Blocked => None,
        _ => None,
    }
}

fn history_facts(
    tasks: &[TaskRow],
    history: &[AgentRunHistoryRow],
    running: &[RunningAgentRow],
) -> Vec<RunArtifactFact> {
    let mut facts = Vec::new();
    let attempt_count = history.len() + running.len();
    if attempt_count > 0 {
        facts.push(RunArtifactFact {
            label: "Attempts".into(),
            value: attempt_count.to_string(),
        });
    }

    let total_tokens = history
        .iter()
        .map(|entry| entry.tokens.total_tokens)
        .sum::<u64>()
        + running
            .iter()
            .map(|entry| entry.tokens.total_tokens)
            .sum::<u64>();
    let total_tokens = if total_tokens > 0 {
        total_tokens
    } else {
        tasks.iter().map(|task| task.total_tokens).sum()
    };
    if total_tokens > 0 {
        facts.push(RunArtifactFact {
            label: "Tokens".into(),
            value: total_tokens.to_string(),
        });
    }

    let runtime_seconds = history
        .iter()
        .filter_map(agent_history_duration_seconds)
        .sum::<i64>()
        + running.iter().map(running_duration_seconds).sum::<i64>();
    if runtime_seconds > 0 {
        facts.push(RunArtifactFact {
            label: "Runtime".into(),
            value: format_duration_seconds(runtime_seconds),
        });
    }

    let mut agents = history
        .iter()
        .map(|entry| entry.agent_name.as_str())
        .chain(running.iter().map(|entry| entry.agent_name.as_str()))
        .collect::<Vec<_>>();
    agents.sort_unstable();
    agents.dedup();
    if !agents.is_empty() {
        facts.push(RunArtifactFact {
            label: "Agents".into(),
            value: agents.join(", "),
        });
    }

    facts
}

fn artifact_facts(run: &RunRow) -> Vec<RunArtifactFact> {
    let mut facts = Vec::new();

    if let Some(repository) = run
        .review_target
        .as_ref()
        .map(|target| format!("{}#{}", target.repository, target.number))
    {
        facts.push(RunArtifactFact {
            label: "Review".into(),
            value: repository,
        });
    }

    if let Some(branch) = metadata_str(run, "branch").or_else(|| {
        run.review_target
            .as_ref()
            .map(|target| target.head_branch.clone())
    }) {
        facts.push(RunArtifactFact {
            label: "Branch".into(),
            value: branch,
        });
    }

    if let Some(commit) = metadata_str(run, "head_sha").or_else(|| {
        run.review_target
            .as_ref()
            .map(|target| short_sha(&target.head_sha))
    }) {
        facts.push(RunArtifactFact {
            label: "Commit".into(),
            value: commit,
        });
    }

    if let Some(changed_files) = metadata_u64(run, "changed_files") {
        facts.push(RunArtifactFact {
            label: "Files".into(),
            value: format!("{changed_files}"),
        });
    }

    let lines_added = metadata_u64(run, "lines_added");
    let lines_removed = metadata_u64(run, "lines_removed");
    if lines_added.is_some() || lines_removed.is_some() {
        facts.push(RunArtifactFact {
            label: "Diff".into(),
            value: format!(
                "+{} / -{}",
                lines_added.unwrap_or(0),
                lines_removed.unwrap_or(0)
            ),
        });
    }

    if let Some(verdict) = metadata_str(run, "verdict") {
        let mut value = verdict;
        if let Some(confidence) = metadata_str(run, "confidence") {
            value.push(' ');
            value.push_str(&confidence);
        }
        if let Some(comments) = metadata_u64(run, "inline_comments") {
            value.push_str(&format!(", {comments} inline"));
        }
        facts.push(RunArtifactFact {
            label: "Review".into(),
            value,
        });
    }

    if let Some(workspace) = metadata_str(run, "workspace_path").or_else(|| {
        run.workspace_path
            .as_ref()
            .map(|path| path.display().to_string())
    }) {
        facts.push(RunArtifactFact {
            label: "Workspace".into(),
            value: workspace,
        });
    }

    facts
}

fn agent_history_duration_seconds(history: &AgentRunHistoryRow) -> Option<i64> {
    let finished_at = history.finished_at.or(history.last_event_at)?;
    Some(
        finished_at
            .signed_duration_since(history.started_at)
            .num_seconds()
            .max(0),
    )
}

fn running_duration_seconds(running: &RunningAgentRow) -> i64 {
    chrono::Utc::now()
        .signed_duration_since(running.started_at)
        .num_seconds()
        .max(0)
}

fn format_duration_seconds(seconds: i64) -> String {
    let seconds = seconds.max(0);
    let hours = seconds / 3_600;
    let minutes = (seconds % 3_600) / 60;
    let remainder = seconds % 60;
    if hours > 0 {
        format!("{hours}h {minutes:02}m")
    } else if minutes > 0 {
        format!("{minutes}m {remainder:02}s")
    } else {
        format!("{remainder}s")
    }
}

fn metadata_str(run: &RunRow, key: &str) -> Option<String> {
    if let Some(value) = run
        .deliverable
        .as_ref()
        .and_then(|deliverable| deliverable.metadata.get(key))
        .and_then(json_value_to_string)
    {
        return Some(value);
    }
    run.steps
        .iter()
        .rev()
        .find_map(|step| step.output.get(key).and_then(json_value_to_string))
}

fn metadata_u64(run: &RunRow, key: &str) -> Option<u64> {
    if let Some(value) = run
        .deliverable
        .as_ref()
        .and_then(|deliverable| deliverable.metadata.get(key))
        .and_then(|value| value.as_u64())
    {
        return Some(value);
    }
    run.steps
        .iter()
        .rev()
        .find_map(|step| step.output.get(key).and_then(|value| value.as_u64()))
}

fn json_value_to_string(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(value) => Some(value.clone()),
        serde_json::Value::Number(value) => Some(value.to_string()),
        serde_json::Value::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}

fn deliverable_kind_label(kind: DeliverableKind) -> &'static str {
    match kind {
        DeliverableKind::GithubPullRequest => "a GitHub pull request",
        DeliverableKind::GitlabMergeRequest => "a GitLab merge request",
        DeliverableKind::LocalBranch => "a local branch",
        DeliverableKind::Patch => "a patch",
        DeliverableKind::PullRequestReview => "a pull request review",
    }
}

fn step_kind_label(kind: StepKind) -> &'static str {
    match kind {
        StepKind::PlannerRun => "planner run",
        StepKind::AgentRun => "agent execution",
        StepKind::Commit => "commit",
        StepKind::Push => "push",
        StepKind::CreatePullRequest => "pull request creation",
        StepKind::ReviewPass => "review pass",
        StepKind::PostReviewComment => "review comment publication",
        StepKind::SendFeedback => "handoff feedback",
        StepKind::AfterOutcomeHooks => "after-outcome hooks",
    }
}

fn short_sha(value: &str) -> String {
    value[..value.floor_char_boundary(12)].to_string()
}

fn quoted(value: &str) -> String {
    format!("`{value}`")
}

fn plural(value: u64) -> &'static str {
    if value == 1 {
        ""
    } else {
        "s"
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use chrono::{Duration, Utc};
    use serde_json::json;

    use super::*;
    use crate::{
        Deliverable, DeliverableDecision, DeliverableStatus, ReviewProviderKind, ReviewTarget,
        RunKind, StepRecord, StepStatus, TaskCategory,
    };

    fn base_run() -> RunRow {
        RunRow {
            repo_id: "penso/polyphony".into(),
            id: "run-1".into(),
            kind: RunKind::IssueDelivery,
            issue_identifier: Some("GH-1".into()),
            title: "Ship the thing".into(),
            status: RunStatus::Delivered,
            task_count: 2,
            tasks_completed: 2,
            deliverable: None,
            has_deliverable: false,
            review_target: None,
            workspace_key: Some("gh-1".into()),
            workspace_path: Some("/tmp/workspace".into()),
            created_at: Utc::now(),
            activity_log: Vec::new(),
            cancel_reason: None,
            blocked_outcome: None,
            steps: Vec::new(),
        }
    }

    fn base_task() -> TaskRow {
        TaskRow {
            repo_id: "penso/polyphony".into(),
            id: "task-1".into(),
            run_id: "run-1".into(),
            title: "Implement".into(),
            description: None,
            activity_log: Vec::new(),
            category: TaskCategory::Coding,
            status: TaskStatus::Completed,
            ordinal: 1,
            agent_name: Some("codex".into()),
            turns_completed: 3,
            total_tokens: 1200,
            started_at: None,
            finished_at: None,
            error: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn base_history() -> AgentRunHistoryRow {
        let started_at = Utc::now() - Duration::minutes(3);
        AgentRunHistoryRow {
            repo_id: "penso/polyphony".into(),
            run_id: Some("run-1".into()),
            issue_id: "issue-1".into(),
            issue_identifier: "GH-1".into(),
            agent_name: "codex".into(),
            model: Some("gpt-5".into()),
            status: crate::AttemptStatus::Succeeded,
            attempt: Some(1),
            max_turns: 4,
            turn_count: 2,
            session_id: Some("sess-1".into()),
            thread_id: Some("thread-1".into()),
            turn_id: None,
            codex_app_server_pid: None,
            last_event: Some("completed".into()),
            last_message: Some("done".into()),
            started_at,
            finished_at: Some(started_at + Duration::seconds(95)),
            last_event_at: Some(started_at + Duration::seconds(95)),
            tokens: crate::TokenUsage {
                input_tokens: 400,
                output_tokens: 200,
                total_tokens: 600,
            },
            workspace_path: Some("/tmp/workspace".into()),
            error: None,
            saved_context: None,
        }
    }

    #[test]
    fn delivered_run_reports_artifacts_and_next_action() {
        let mut run = base_run();
        run.deliverable = Some(Deliverable {
            kind: DeliverableKind::GithubPullRequest,
            status: DeliverableStatus::Open,
            url: Some("https://github.com/penso/polyphony/pull/7".into()),
            decision: DeliverableDecision::Waiting,
            title: Some("feat: ship the thing".into()),
            description: None,
            metadata: std::collections::HashMap::from([
                ("changed_files".into(), json!(5)),
                ("lines_added".into(), json!(120)),
                ("lines_removed".into(), json!(14)),
                ("head_sha".into(), json!("0123456789abcdef")),
                ("branch".into(), json!("feat/ship-the-thing")),
            ]),
        });
        run.has_deliverable = true;

        let insight = build_run_insight(&run, &[base_task()], &[base_history()], &[]);

        assert!(insight.summary.contains("GitHub pull request"));
        assert_eq!(
            insight.next_action.as_deref(),
            Some("Review the deliverable, then accept or reject it.")
        );
        assert!(
            insight
                .artifact_facts
                .iter()
                .any(|fact| fact.label == "Branch" && fact.value == "feat/ship-the-thing")
        );
        assert!(
            insight
                .artifact_facts
                .iter()
                .any(|fact| fact.label == "Files" && fact.value == "5")
        );
        assert!(
            insight
                .history_facts
                .iter()
                .any(|fact| fact.label == "Attempts" && fact.value == "1")
        );
        assert!(
            insight
                .history_facts
                .iter()
                .any(|fact| fact.label == "Tokens" && fact.value == "600")
        );
    }

    #[test]
    fn failed_run_prefers_step_failure_reason() {
        let mut run = base_run();
        run.status = RunStatus::Failed;
        let mut step = StepRecord::new(StepKind::CreatePullRequest, 3);
        step.status = StepStatus::Failed;
        step.error = Some("tracker authentication failed".into());
        run.steps = vec![step];

        let insight = build_run_insight(&run, &[base_task()], &[base_history()], &[]);

        assert_eq!(insight.summary, "Failed during pull request creation.");
        assert_eq!(
            insight.stop_reason.as_deref(),
            Some("tracker authentication failed")
        );
        assert_eq!(
            insight.next_action.as_deref(),
            Some("Inspect the failure, then retry the run.")
        );
    }

    #[test]
    fn review_run_reports_verdict_metadata() {
        let mut run = base_run();
        run.kind = RunKind::PullRequestReview;
        run.review_target = Some(ReviewTarget {
            provider: ReviewProviderKind::Github,
            repository: "penso/polyphony".into(),
            number: 42,
            url: Some("https://github.com/penso/polyphony/pull/42".into()),
            base_branch: "main".into(),
            head_branch: "feat/review".into(),
            head_sha: "abcdef0123456789".into(),
            checkout_ref: None,
        });
        run.deliverable = Some(Deliverable {
            kind: DeliverableKind::PullRequestReview,
            status: DeliverableStatus::Reviewed,
            url: Some("https://github.com/penso/polyphony/pull/42".into()),
            decision: DeliverableDecision::Accepted,
            title: Some("Review: approve".into()),
            description: Some("Looks good.".into()),
            metadata: std::collections::HashMap::from([
                ("verdict".into(), json!("approve")),
                ("inline_comments".into(), json!(3)),
                ("confidence".into(), json!("4/5")),
            ]),
        });
        run.has_deliverable = true;

        let insight = build_run_insight(&run, &[], &[base_history()], &[]);

        assert!(insight.summary.contains("approve verdict"));
        assert!(
            insight
                .artifact_facts
                .iter()
                .any(|fact| fact.label == "Review" && fact.value.contains("approve"))
        );
    }

    #[test]
    fn run_matching_prefers_run_id_when_present() {
        let run = base_run();
        let matching = base_history();
        let mut different_issue_same_identifier = base_history();
        different_issue_same_identifier.run_id = Some("run-2".into());

        assert!(agent_history_matches_run(&run, &matching));
        assert!(!agent_history_matches_run(
            &run,
            &different_issue_same_identifier
        ));
    }
}
