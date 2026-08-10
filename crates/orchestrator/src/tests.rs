#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::{
    collections::{HashMap, VecDeque},
    fs,
    path::Path,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use polyphony_core::{
    AddIssueCommentRequest, AgentSession, CreateIssueRequest, Deliverable, DeliverableDecision,
    DeliverableKind, DeliverableStatus, DispatchMode, IssueAuthor, IssueComment, IssueStateUpdate,
    PullRequestRef, StepStatus, StoreBootstrap, UpdateIssueRequest, Workspace,
    WorkspaceCommitResult, WorkspaceRequest,
};
use polyphony_workflow::load_workflow;
use serde_json::json;
use tokio::{
    sync::{Notify, watch},
    time::timeout,
};

use crate::{helpers::*, prelude::*, *};

#[derive(Clone)]
struct TestTracker {
    issues: Arc<Mutex<HashMap<String, Issue>>>,
    workflow_updates: Arc<Mutex<Vec<String>>>,
    workflow_status_error: Arc<Mutex<Option<String>>>,
    fetch_by_ids_calls: Arc<Mutex<u32>>,
    issue_updates: Arc<Mutex<Vec<UpdateIssueRequest>>>,
    acknowledged_issues: Arc<Mutex<Vec<String>>>,
    created_issues: Arc<Mutex<Vec<CreateIssueRequest>>>,
    comments: Arc<Mutex<Vec<AddIssueCommentRequest>>>,
    write_order: Arc<Mutex<Vec<String>>>,
}

#[derive(Clone)]
struct DelayedCleanupTracker {
    issues: Arc<Vec<Issue>>,
    cleanup_gate: Arc<Notify>,
}

#[async_trait]
impl IssueTracker for DelayedCleanupTracker {
    fn component_key(&self) -> String {
        "tracker:delayed-cleanup".into()
    }

    async fn fetch_candidate_issues(
        &self,
        _query: &polyphony_core::TrackerQuery,
    ) -> Result<Vec<Issue>, polyphony_core::Error> {
        Ok(self.issues.as_ref().clone())
    }

    async fn fetch_issues_by_states(
        &self,
        _project_slug: Option<&str>,
        _states: &[String],
    ) -> Result<Vec<Issue>, polyphony_core::Error> {
        self.cleanup_gate.notified().await;
        Ok(Vec::new())
    }

    async fn fetch_issues_by_ids(
        &self,
        issue_ids: &[String],
    ) -> Result<Vec<Issue>, polyphony_core::Error> {
        Ok(self
            .issues
            .iter()
            .filter(|issue| issue_ids.contains(&issue.id))
            .cloned()
            .collect())
    }

    async fn fetch_issue_states_by_ids(
        &self,
        issue_ids: &[String],
    ) -> Result<Vec<polyphony_core::IssueStateUpdate>, polyphony_core::Error> {
        Ok(self
            .issues
            .iter()
            .filter(|issue| issue_ids.contains(&issue.id))
            .map(|issue| IssueStateUpdate {
                id: issue.id.clone(),
                identifier: issue.identifier.clone(),
                state: issue.state.clone(),
                updated_at: issue.updated_at,
            })
            .collect())
    }
}

impl TestTracker {
    fn new(issues: Vec<Issue>) -> Self {
        Self {
            issues: Arc::new(Mutex::new(
                issues
                    .into_iter()
                    .map(|issue| (issue.id.clone(), issue))
                    .collect(),
            )),
            workflow_updates: Arc::new(Mutex::new(Vec::new())),
            workflow_status_error: Arc::new(Mutex::new(None)),
            fetch_by_ids_calls: Arc::new(Mutex::new(0)),
            issue_updates: Arc::new(Mutex::new(Vec::new())),
            acknowledged_issues: Arc::new(Mutex::new(Vec::new())),
            created_issues: Arc::new(Mutex::new(Vec::new())),
            comments: Arc::new(Mutex::new(Vec::new())),
            write_order: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn recorded_workflow_updates(&self) -> Vec<String> {
        self.workflow_updates.lock().unwrap().clone()
    }

    fn fail_workflow_status_updates(self, error: impl Into<String>) -> Self {
        *self.workflow_status_error.lock().unwrap() = Some(error.into());
        self
    }

    fn fetch_by_ids_calls(&self) -> u32 {
        *self.fetch_by_ids_calls.lock().unwrap()
    }

    fn recorded_issue_updates(&self) -> Vec<UpdateIssueRequest> {
        self.issue_updates.lock().unwrap().clone()
    }

    fn acknowledged_issues(&self) -> Vec<String> {
        self.acknowledged_issues.lock().unwrap().clone()
    }

    fn recorded_create_issues(&self) -> Vec<CreateIssueRequest> {
        self.created_issues.lock().unwrap().clone()
    }

    fn recorded_comments(&self) -> Vec<AddIssueCommentRequest> {
        self.comments.lock().unwrap().clone()
    }

    fn write_order(&self) -> Vec<String> {
        self.write_order.lock().unwrap().clone()
    }
}

/// Models the error emitted by the GitHub Project-v2 adapter when its configured
/// Status field is blank.  The orchestrator must treat this tracker-poll error
/// as a hard dispatch boundary.
#[derive(Clone)]
struct GithubBlankProjectStatusTracker {
    project_status: String,
    candidate_polls: Arc<Mutex<u32>>,
    acknowledgements: Arc<Mutex<Vec<String>>>,
}

impl GithubBlankProjectStatusTracker {
    fn with_project_status(project_status: impl Into<String>) -> Self {
        Self {
            project_status: project_status.into(),
            candidate_polls: Arc::new(Mutex::new(0)),
            acknowledgements: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn candidate_polls(&self) -> u32 {
        *self.candidate_polls.lock().unwrap()
    }

    fn acknowledgements(&self) -> Vec<String> {
        self.acknowledgements.lock().unwrap().clone()
    }
}

#[async_trait]
impl IssueTracker for GithubBlankProjectStatusTracker {
    fn component_key(&self) -> String {
        "tracker:github-project-status-mock".into()
    }

    async fn fetch_candidate_issues(
        &self,
        _query: &polyphony_core::TrackerQuery,
    ) -> Result<Vec<Issue>, polyphony_core::Error> {
        *self.candidate_polls.lock().unwrap() += 1;
        if self.project_status.trim().is_empty() {
            return Err(polyphony_core::Error::Adapter(
                "GitHub Project Status is missing or empty; refusing to use GitHub open/closed state"
                    .into(),
            ));
        }
        Ok(Vec::new())
    }

    async fn fetch_issues_by_states(
        &self,
        _project_slug: Option<&str>,
        _states: &[String],
    ) -> Result<Vec<Issue>, polyphony_core::Error> {
        Ok(Vec::new())
    }

    async fn fetch_issues_by_ids(
        &self,
        _issue_ids: &[String],
    ) -> Result<Vec<Issue>, polyphony_core::Error> {
        Ok(Vec::new())
    }

    async fn fetch_issue_states_by_ids(
        &self,
        _issue_ids: &[String],
    ) -> Result<Vec<polyphony_core::IssueStateUpdate>, polyphony_core::Error> {
        Ok(Vec::new())
    }

    async fn acknowledge_issue(&self, issue: &Issue) -> Result<(), polyphony_core::Error> {
        self.acknowledgements.lock().unwrap().push(issue.id.clone());
        Ok(())
    }
}

#[async_trait]
impl IssueTracker for TestTracker {
    fn component_key(&self) -> String {
        "tracker:test".into()
    }

    async fn fetch_candidate_issues(
        &self,
        _query: &polyphony_core::TrackerQuery,
    ) -> Result<Vec<Issue>, polyphony_core::Error> {
        Ok(self.issues.lock().unwrap().values().cloned().collect())
    }

    async fn fetch_issues_by_states(
        &self,
        _project_slug: Option<&str>,
        states: &[String],
    ) -> Result<Vec<Issue>, polyphony_core::Error> {
        let normalized = states
            .iter()
            .map(|state| state.to_ascii_lowercase())
            .collect::<Vec<_>>();
        Ok(self
            .issues
            .lock()
            .unwrap()
            .values()
            .filter(|issue| normalized.contains(&issue.state.to_ascii_lowercase()))
            .cloned()
            .collect())
    }

    async fn fetch_issues_by_ids(
        &self,
        issue_ids: &[String],
    ) -> Result<Vec<Issue>, polyphony_core::Error> {
        *self.fetch_by_ids_calls.lock().unwrap() += 1;
        let issues = self.issues.lock().unwrap();
        Ok(issue_ids
            .iter()
            .filter_map(|issue_id| issues.get(issue_id))
            .cloned()
            .collect())
    }

    async fn fetch_issue_states_by_ids(
        &self,
        issue_ids: &[String],
    ) -> Result<Vec<polyphony_core::IssueStateUpdate>, polyphony_core::Error> {
        let issues = self.issues.lock().unwrap();
        Ok(issue_ids
            .iter()
            .filter_map(|issue_id| issues.get(issue_id))
            .map(|issue| polyphony_core::IssueStateUpdate {
                id: issue.id.clone(),
                identifier: issue.identifier.clone(),
                state: issue.state.clone(),
                updated_at: issue.updated_at,
            })
            .collect())
    }

    async fn update_issue_workflow_status(
        &self,
        _issue: &Issue,
        status: &str,
    ) -> Result<(), polyphony_core::Error> {
        if let Some(error) = self.workflow_status_error.lock().unwrap().clone() {
            return Err(polyphony_core::Error::Adapter(error));
        }
        self.workflow_updates
            .lock()
            .unwrap()
            .push(status.to_string());
        self.write_order
            .lock()
            .unwrap()
            .push(format!("workflow:{status}"));
        Ok(())
    }

    async fn update_issue(
        &self,
        request: &UpdateIssueRequest,
    ) -> Result<Issue, polyphony_core::Error> {
        self.issue_updates.lock().unwrap().push(request.clone());
        let mut issues = self.issues.lock().unwrap();
        let issue = issues.get_mut(&request.id).ok_or_else(|| {
            polyphony_core::Error::Adapter(format!("issue {} not found", request.id))
        })?;
        if let Some(state) = &request.state {
            issue.state = state.clone();
        }
        if let Some(title) = &request.title {
            issue.title = title.clone();
        }
        if let Some(description) = &request.description {
            issue.description = Some(description.clone());
        }
        if let Some(priority) = request.priority {
            issue.priority = Some(priority);
        }
        issue.updated_at = Some(Utc::now());
        Ok(issue.clone())
    }

    async fn comment_on_issue(
        &self,
        request: &AddIssueCommentRequest,
    ) -> Result<IssueComment, polyphony_core::Error> {
        self.comments.lock().unwrap().push(request.clone());
        self.write_order.lock().unwrap().push("comment".into());
        Ok(IssueComment {
            id: format!("comment-{}", self.comments.lock().unwrap().len()),
            body: request.body.clone(),
            author: None,
            url: Some(format!(
                "https://tracker.test/issues/{}/comments/{}",
                request.id,
                self.comments.lock().unwrap().len()
            )),
            created_at: Some(Utc::now()),
            updated_at: None,
        })
    }

    async fn acknowledge_issue(&self, issue: &Issue) -> Result<(), polyphony_core::Error> {
        self.acknowledged_issues
            .lock()
            .unwrap()
            .push(issue.id.clone());
        Ok(())
    }

    async fn create_issue(
        &self,
        request: &CreateIssueRequest,
    ) -> Result<Issue, polyphony_core::Error> {
        let next = {
            let mut created_issues = self.created_issues.lock().unwrap();
            created_issues.push(request.clone());
            created_issues.len()
        };
        let issue = Issue {
            id: format!("created-{next}"),
            identifier: format!("CREATED-{next}"),
            title: request.title.clone(),
            description: request.description.clone(),
            priority: request.priority,
            state: "Todo".into(),
            branch_name: None,
            url: None,
            author: None,
            labels: request.labels.clone(),
            comments: Vec::new(),
            blocked_by: Vec::new(),
            approval_state: DispatchApprovalState::Approved,
            parent_id: request.parent_id.clone(),
            created_at: Some(Utc::now()),
            updated_at: Some(Utc::now()),
        };
        self.issues
            .lock()
            .unwrap()
            .insert(issue.id.clone(), issue.clone());
        Ok(issue)
    }
}

struct NoopAgent;

#[async_trait]
impl AgentRuntime for NoopAgent {
    fn component_key(&self) -> String {
        "provider:test".into()
    }

    async fn run(
        &self,
        _spec: AgentRunSpec,
        _event_tx: mpsc::UnboundedSender<AgentEvent>,
    ) -> Result<AgentRunResult, polyphony_core::Error> {
        Ok(AgentRunResult::succeeded(1))
    }
}

#[derive(Clone, Default)]
struct RecordingPullRequestCommenter {
    comments: Arc<Mutex<Vec<(PullRequestRef, String)>>>,
    reviews: Arc<
        Mutex<
            Vec<(
                PullRequestRef,
                String,
                Vec<PullRequestReviewComment>,
                String,
            )>,
        >,
    >,
}

impl RecordingPullRequestCommenter {
    fn comment_bodies(&self) -> Vec<String> {
        self.comments
            .lock()
            .unwrap()
            .iter()
            .map(|(_, body)| body.clone())
            .collect()
    }

    fn reviews(
        &self,
    ) -> Vec<(
        PullRequestRef,
        String,
        Vec<PullRequestReviewComment>,
        String,
    )> {
        self.reviews.lock().unwrap().clone()
    }
}

#[async_trait]
impl PullRequestCommenter for RecordingPullRequestCommenter {
    fn component_key(&self) -> String {
        "github:test-comments".into()
    }

    async fn comment_on_pull_request(
        &self,
        pull_request: &PullRequestRef,
        body: &str,
    ) -> Result<(), polyphony_core::Error> {
        self.comments
            .lock()
            .unwrap()
            .push((pull_request.clone(), body.to_string()));
        Ok(())
    }

    async fn sync_pull_request_comment(
        &self,
        pull_request: &PullRequestRef,
        marker: &str,
        body: &str,
    ) -> Result<(), polyphony_core::Error> {
        let mut comments = self.comments.lock().unwrap();
        if let Some((_, existing_body)) = comments
            .iter_mut()
            .find(|(_, existing_body)| existing_body.contains(marker))
        {
            *existing_body = body.to_string();
        } else {
            comments.push((pull_request.clone(), body.to_string()));
        }
        Ok(())
    }

    async fn sync_pull_request_review(
        &self,
        pull_request: &PullRequestRef,
        marker: &str,
        body: &str,
        comments: &[PullRequestReviewComment],
        commit_sha: &str,
        _verdict: polyphony_core::ReviewVerdict,
    ) -> Result<(), polyphony_core::Error> {
        let mut reviews = self.reviews.lock().unwrap();
        if reviews.iter().any(|review| review.1.contains(marker)) {
            return Ok(());
        }
        reviews.push((
            pull_request.clone(),
            body.to_string(),
            comments.to_vec(),
            commit_sha.to_string(),
        ));
        Ok(())
    }
}

#[derive(Clone)]
struct NamedTracker {
    component: String,
    issues: Arc<Mutex<HashMap<String, Issue>>>,
}

impl NamedTracker {
    fn new(component: impl Into<String>, issues: Vec<Issue>) -> Self {
        Self {
            component: component.into(),
            issues: Arc::new(Mutex::new(
                issues
                    .into_iter()
                    .map(|issue| (issue.id.clone(), issue))
                    .collect(),
            )),
        }
    }
}

#[async_trait]
impl IssueTracker for NamedTracker {
    fn component_key(&self) -> String {
        self.component.clone()
    }

    async fn fetch_candidate_issues(
        &self,
        _query: &polyphony_core::TrackerQuery,
    ) -> Result<Vec<Issue>, polyphony_core::Error> {
        Ok(self.issues.lock().unwrap().values().cloned().collect())
    }

    async fn fetch_issues_by_states(
        &self,
        _project_slug: Option<&str>,
        states: &[String],
    ) -> Result<Vec<Issue>, polyphony_core::Error> {
        let normalized = states
            .iter()
            .map(|state| state.to_ascii_lowercase())
            .collect::<Vec<_>>();
        Ok(self
            .issues
            .lock()
            .unwrap()
            .values()
            .filter(|issue| normalized.contains(&issue.state.to_ascii_lowercase()))
            .cloned()
            .collect())
    }

    async fn fetch_issues_by_ids(
        &self,
        issue_ids: &[String],
    ) -> Result<Vec<Issue>, polyphony_core::Error> {
        let issues = self.issues.lock().unwrap();
        Ok(issue_ids
            .iter()
            .filter_map(|issue_id| issues.get(issue_id))
            .cloned()
            .collect())
    }

    async fn fetch_issue_states_by_ids(
        &self,
        issue_ids: &[String],
    ) -> Result<Vec<IssueStateUpdate>, polyphony_core::Error> {
        let issues = self.issues.lock().unwrap();
        Ok(issue_ids
            .iter()
            .filter_map(|issue_id| issues.get(issue_id))
            .map(|issue| IssueStateUpdate {
                id: issue.id.clone(),
                identifier: issue.identifier.clone(),
                state: issue.state.clone(),
                updated_at: issue.updated_at,
            })
            .collect())
    }
}

#[derive(Clone)]
struct NamedAgent {
    component: String,
}

impl NamedAgent {
    fn new(component: impl Into<String>) -> Self {
        Self {
            component: component.into(),
        }
    }
}

#[async_trait]
impl AgentRuntime for NamedAgent {
    fn component_key(&self) -> String {
        self.component.clone()
    }

    async fn run(
        &self,
        _spec: AgentRunSpec,
        _event_tx: mpsc::UnboundedSender<AgentEvent>,
    ) -> Result<AgentRunResult, polyphony_core::Error> {
        Ok(AgentRunResult::succeeded(1))
    }
}

#[derive(Clone, Default)]
struct RecordingSessionAgent {
    prompts: Arc<Mutex<Vec<String>>>,
    session_starts: Arc<Mutex<u32>>,
    stops: Arc<Mutex<u32>>,
    final_issue_state: Option<String>,
}

impl RecordingSessionAgent {
    fn prompts(&self) -> Vec<String> {
        self.prompts.lock().unwrap().clone()
    }

    fn session_starts(&self) -> u32 {
        *self.session_starts.lock().unwrap()
    }

    fn stops(&self) -> u32 {
        *self.stops.lock().unwrap()
    }

    fn with_final_issue_state(final_issue_state: impl Into<String>) -> Self {
        Self {
            final_issue_state: Some(final_issue_state.into()),
            ..Self::default()
        }
    }
}

struct RecordingSession {
    prompts: Arc<Mutex<Vec<String>>>,
    stops: Arc<Mutex<u32>>,
    final_issue_state: Option<String>,
}

#[derive(Clone, Default)]
struct BlockingSessionAgent {
    started: Arc<Notify>,
    stops: Arc<Mutex<u32>>,
}

impl BlockingSessionAgent {
    fn stops(&self) -> u32 {
        *self.stops.lock().unwrap()
    }
}

struct BlockingSession {
    started: Arc<Notify>,
    stops: Arc<Mutex<u32>>,
}

#[derive(Clone, Default)]
struct FailingStopSessionAgent {
    started: Arc<Notify>,
}

struct FailingStopSession {
    started: Arc<Notify>,
}

#[derive(Clone, Default)]
struct FailingCancellationCleanupAgent {
    started: Arc<Notify>,
}

#[async_trait]
impl AgentRuntime for FailingCancellationCleanupAgent {
    fn component_key(&self) -> String {
        "provider:failing-cancellation-cleanup-test".into()
    }

    async fn run(
        &self,
        _spec: AgentRunSpec,
        _event_tx: mpsc::UnboundedSender<AgentEvent>,
    ) -> Result<AgentRunResult, polyphony_core::Error> {
        self.started.notify_one();
        std::future::pending().await
    }

    async fn confirm_cancellation(
        &self,
        _spec: &AgentRunSpec,
    ) -> Result<(), polyphony_core::Error> {
        Err(polyphony_core::Error::Adapter(
            "injected owned PTY cleanup failure".into(),
        ))
    }
}

#[derive(Clone, Default)]
struct StartupBlockingProcessAgent {
    started: Arc<Notify>,
    pid: Arc<Mutex<Option<u32>>>,
}

#[async_trait]
impl AgentRuntime for StartupBlockingProcessAgent {
    fn component_key(&self) -> String {
        "provider:startup-blocking-process-test".into()
    }

    async fn start_session(
        &self,
        _spec: AgentRunSpec,
        _event_tx: mpsc::UnboundedSender<AgentEvent>,
    ) -> Result<Option<Box<dyn AgentSession>>, polyphony_core::Error> {
        let mut command = tokio::process::Command::new("sh");
        command
            .arg("-c")
            .arg("while :; do sleep 1; done")
            .kill_on_drop(true);
        let child = command
            .spawn()
            .map_err(|error| polyphony_core::Error::Adapter(error.to_string()))?;
        *self.pid.lock().unwrap() = child.id();
        self.started.notify_one();
        // Simulate an app-server that spawned but never completes its startup
        // handshake. Dropping this future must terminate `child`.
        std::future::pending::<Result<Option<Box<dyn AgentSession>>, polyphony_core::Error>>().await
    }

    async fn run(
        &self,
        _spec: AgentRunSpec,
        _event_tx: mpsc::UnboundedSender<AgentEvent>,
    ) -> Result<AgentRunResult, polyphony_core::Error> {
        unreachable!("run_worker_attempt starts a session first")
    }
}

#[async_trait]
impl AgentSession for BlockingSession {
    async fn run_turn(&mut self, _prompt: String) -> Result<AgentRunResult, polyphony_core::Error> {
        self.started.notify_one();
        std::future::pending().await
    }

    async fn stop(&mut self) -> Result<(), polyphony_core::Error> {
        *self.stops.lock().unwrap() += 1;
        Ok(())
    }
}

#[async_trait]
impl AgentRuntime for BlockingSessionAgent {
    fn component_key(&self) -> String {
        "provider:blocking-session-test".into()
    }

    async fn start_session(
        &self,
        _spec: AgentRunSpec,
        _event_tx: mpsc::UnboundedSender<AgentEvent>,
    ) -> Result<Option<Box<dyn AgentSession>>, polyphony_core::Error> {
        Ok(Some(Box::new(BlockingSession {
            started: self.started.clone(),
            stops: self.stops.clone(),
        })))
    }

    async fn run(
        &self,
        _spec: AgentRunSpec,
        _event_tx: mpsc::UnboundedSender<AgentEvent>,
    ) -> Result<AgentRunResult, polyphony_core::Error> {
        Err(polyphony_core::Error::Adapter(
            "run() should not be used when live sessions are available".into(),
        ))
    }
}

#[async_trait]
impl AgentSession for FailingStopSession {
    async fn run_turn(&mut self, _prompt: String) -> Result<AgentRunResult, polyphony_core::Error> {
        self.started.notify_one();
        std::future::pending().await
    }

    async fn stop(&mut self) -> Result<(), polyphony_core::Error> {
        Err(polyphony_core::Error::Adapter(
            "simulated process termination failure".into(),
        ))
    }
}

#[async_trait]
impl AgentRuntime for FailingStopSessionAgent {
    fn component_key(&self) -> String {
        "provider:failing-stop-session-test".into()
    }

    async fn start_session(
        &self,
        _spec: AgentRunSpec,
        _event_tx: mpsc::UnboundedSender<AgentEvent>,
    ) -> Result<Option<Box<dyn AgentSession>>, polyphony_core::Error> {
        Ok(Some(Box::new(FailingStopSession {
            started: self.started.clone(),
        })))
    }

    async fn run(
        &self,
        _spec: AgentRunSpec,
        _event_tx: mpsc::UnboundedSender<AgentEvent>,
    ) -> Result<AgentRunResult, polyphony_core::Error> {
        unreachable!("run_worker_attempt starts a session first")
    }
}

#[async_trait]
impl AgentSession for RecordingSession {
    async fn run_turn(&mut self, prompt: String) -> Result<AgentRunResult, polyphony_core::Error> {
        self.prompts.lock().unwrap().push(prompt);
        Ok(AgentRunResult {
            status: AttemptStatus::Succeeded,
            turns_completed: 1,
            error: None,
            final_issue_state: self.final_issue_state.clone(),
        })
    }

    async fn stop(&mut self) -> Result<(), polyphony_core::Error> {
        *self.stops.lock().unwrap() += 1;
        Ok(())
    }
}

#[async_trait]
impl AgentRuntime for RecordingSessionAgent {
    fn component_key(&self) -> String {
        "provider:session-test".into()
    }

    async fn start_session(
        &self,
        _spec: AgentRunSpec,
        _event_tx: mpsc::UnboundedSender<AgentEvent>,
    ) -> Result<Option<Box<dyn AgentSession>>, polyphony_core::Error> {
        *self.session_starts.lock().unwrap() += 1;
        Ok(Some(Box::new(RecordingSession {
            prompts: self.prompts.clone(),
            stops: self.stops.clone(),
            final_issue_state: self.final_issue_state.clone(),
        })))
    }

    async fn run(
        &self,
        _spec: AgentRunSpec,
        _event_tx: mpsc::UnboundedSender<AgentEvent>,
    ) -> Result<AgentRunResult, polyphony_core::Error> {
        Err(polyphony_core::Error::Adapter(
            "run() should not be used when live sessions are available".into(),
        ))
    }
}

#[derive(Clone)]
struct SequencedPullRequestEventSource {
    batches: Arc<Mutex<VecDeque<Vec<PullRequestEvent>>>>,
}

impl SequencedPullRequestEventSource {
    fn new(batches: Vec<Vec<PullRequestEvent>>) -> Self {
        Self {
            batches: Arc::new(Mutex::new(batches.into())),
        }
    }
}

#[async_trait]
impl PullRequestEventSource for SequencedPullRequestEventSource {
    fn component_key(&self) -> String {
        "github:test-pr-events".into()
    }

    async fn fetch_events(&self) -> Result<Vec<PullRequestEvent>, polyphony_core::Error> {
        Ok(self.batches.lock().unwrap().pop_front().unwrap_or_default())
    }
}

struct SequencedStateTracker {
    issue: Issue,
    states: Arc<Mutex<VecDeque<String>>>,
}

impl SequencedStateTracker {
    fn new(issue: Issue, states: Vec<&str>) -> Self {
        Self {
            issue,
            states: Arc::new(Mutex::new(states.into_iter().map(str::to_string).collect())),
        }
    }
}

#[async_trait]
impl IssueTracker for SequencedStateTracker {
    fn component_key(&self) -> String {
        "tracker:sequence".into()
    }

    async fn fetch_candidate_issues(
        &self,
        _query: &polyphony_core::TrackerQuery,
    ) -> Result<Vec<Issue>, polyphony_core::Error> {
        Ok(vec![self.issue.clone()])
    }

    async fn fetch_issues_by_states(
        &self,
        _project_slug: Option<&str>,
        _states: &[String],
    ) -> Result<Vec<Issue>, polyphony_core::Error> {
        Ok(vec![self.issue.clone()])
    }

    async fn fetch_issues_by_ids(
        &self,
        issue_ids: &[String],
    ) -> Result<Vec<Issue>, polyphony_core::Error> {
        if issue_ids.iter().any(|issue_id| issue_id == &self.issue.id) {
            Ok(vec![self.issue.clone()])
        } else {
            Ok(Vec::new())
        }
    }

    async fn fetch_issue_states_by_ids(
        &self,
        issue_ids: &[String],
    ) -> Result<Vec<IssueStateUpdate>, polyphony_core::Error> {
        if !issue_ids.iter().any(|issue_id| issue_id == &self.issue.id) {
            return Ok(Vec::new());
        }
        let state = self
            .states
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| self.issue.state.clone());
        Ok(vec![IssueStateUpdate {
            id: self.issue.id.clone(),
            identifier: self.issue.identifier.clone(),
            state,
            updated_at: self.issue.updated_at,
        }])
    }
}

#[derive(Clone, Default)]
struct RecordingProvisioner {
    cleaned: Arc<Mutex<Vec<String>>>,
}

impl RecordingProvisioner {
    fn cleaned_issue_identifiers(&self) -> Vec<String> {
        self.cleaned.lock().unwrap().clone()
    }
}

#[async_trait]
impl WorkspaceProvisioner for RecordingProvisioner {
    fn component_key(&self) -> String {
        "workspace:test".into()
    }

    async fn ensure_workspace(
        &self,
        request: WorkspaceRequest,
    ) -> Result<Workspace, polyphony_core::Error> {
        tokio::fs::create_dir_all(&request.workspace_path)
            .await
            .map_err(|error| polyphony_core::Error::Adapter(error.to_string()))?;
        Ok(Workspace {
            path: request.workspace_path,
            workspace_key: request.workspace_key,
            created_now: false,
            branch_name: request.branch_name,
        })
    }

    async fn cleanup_workspace(
        &self,
        request: WorkspaceRequest,
    ) -> Result<(), polyphony_core::Error> {
        self.cleaned
            .lock()
            .unwrap()
            .push(request.issue_identifier.clone());
        if tokio::fs::try_exists(&request.workspace_path)
            .await
            .map_err(|error| polyphony_core::Error::Adapter(error.to_string()))?
        {
            tokio::fs::remove_dir_all(&request.workspace_path)
                .await
                .map_err(|error| polyphony_core::Error::Adapter(error.to_string()))?;
        }
        Ok(())
    }
}

#[derive(Clone)]
struct FailingProvisioner {
    message: String,
}

#[async_trait]
impl WorkspaceProvisioner for FailingProvisioner {
    fn component_key(&self) -> String {
        "workspace:failing".into()
    }

    async fn ensure_workspace(
        &self,
        _request: WorkspaceRequest,
    ) -> Result<Workspace, polyphony_core::Error> {
        Err(polyphony_core::Error::Adapter(self.message.clone()))
    }

    async fn cleanup_workspace(
        &self,
        _request: WorkspaceRequest,
    ) -> Result<(), polyphony_core::Error> {
        Ok(())
    }
}

#[derive(Clone, Default)]
struct ScriptedPipelineAgent {
    calls: Arc<Mutex<Vec<(String, String)>>>,
}

/// Disposable fake worker for the independent-QA delivery contract.  It never
/// calls a provider or repository automation: role names alone determine the
/// scripted lifecycle and the QA reports are returned as durable evidence.
#[derive(Clone, Default)]
struct ClosedLoopQaFixtureAgent {
    calls: Arc<Mutex<Vec<String>>>,
    qa_attempts: Arc<Mutex<u32>>,
}

impl ClosedLoopQaFixtureAgent {
    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl AgentRuntime for ClosedLoopQaFixtureAgent {
    fn component_key(&self) -> String {
        "provider:closed-loop-qa-fixture".into()
    }

    async fn run(
        &self,
        spec: AgentRunSpec,
        _event_tx: mpsc::UnboundedSender<AgentEvent>,
    ) -> Result<AgentRunResult, polyphony_core::Error> {
        self.calls.lock().unwrap().push(spec.agent.name.clone());
        let final_issue_state = if spec.agent.name == "qa" {
            let mut attempts = self.qa_attempts.lock().unwrap();
            *attempts += 1;
            Some(if *attempts == 1 {
                "QA FAIL: fixture found the implementation marker is incomplete\n\
                 tests run: focused fixture\n\
                 checks: 1, 2, 3, 4, 5\n\
                 realistic: yes\n\
                 material: yes\n\
                 risks: lost evidence\n\
                 small fix: yes\n\
                 recommendation: remediate"
                    .into()
            } else {
                "QA PASS: fixture confirmed the repair marker and focused checks\n\
                 tests run: focused fixture\n\
                 checks: 1, 2, 3, 4, 5"
                    .into()
            })
        } else if spec.agent.name == "repair" {
            Some(
                "REPAIR NOTE:\n\
                  what fixed: completed the missing fixture marker\n\
                  commit: def456\n\
                  tests run: focused fixture\n\
                  recheck: independent QA checks 1 through 5\n\
                  checks: 3, 4"
                    .into(),
            )
        } else {
            Some(
                "IMPLEMENTATION NOTE:\n\
                  what changed: added the fixture implementation marker\n\
                  commit: abc123\n\
                  tests run: focused fixture\n\
                  checks: 1, 2"
                    .into(),
            )
        };
        Ok(AgentRunResult {
            status: AttemptStatus::Succeeded,
            turns_completed: 1,
            error: None,
            final_issue_state,
        })
    }
}

impl ScriptedPipelineAgent {
    fn recorded_agent_names(&self) -> Vec<String> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .map(|(agent_name, _)| agent_name.clone())
            .collect()
    }

    fn recorded_calls(&self) -> Vec<(String, String)> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl AgentRuntime for ScriptedPipelineAgent {
    fn component_key(&self) -> String {
        "provider:scripted-pipeline".into()
    }

    async fn run(
        &self,
        spec: AgentRunSpec,
        _event_tx: mpsc::UnboundedSender<AgentEvent>,
    ) -> Result<AgentRunResult, polyphony_core::Error> {
        self.calls
            .lock()
            .unwrap()
            .push((spec.agent.name.clone(), spec.prompt.clone()));
        let polyphony_dir = spec.workspace_path.join(".polyphony");
        tokio::fs::create_dir_all(&polyphony_dir)
            .await
            .map_err(|error| polyphony_core::Error::Adapter(error.to_string()))?;

        if spec.agent.name == "router" {
            let plan = json!({
                "tasks": [{
                    "title": "Create the missing file",
                    "category": "coding",
                    "description": "Add the repository marker file requested by the issue.",
                    "agent": "implementer"
                }]
            });
            tokio::fs::write(polyphony_dir.join("plan.json"), plan.to_string())
                .await
                .map_err(|error| polyphony_core::Error::Adapter(error.to_string()))?;
            return Ok(AgentRunResult::succeeded(1));
        }

        if spec.prompt.contains("Review the current branch against") {
            tokio::fs::write(
                polyphony_dir.join("review.md"),
                "Summary\n\nAutomated review found no blockers.",
            )
            .await
            .map_err(|error| polyphony_core::Error::Adapter(error.to_string()))?;
            return Ok(AgentRunResult::succeeded(1));
        }

        tokio::fs::write(
            spec.workspace_path.join("e2e-pr.txt"),
            "polyphony end-to-end dogfood\n",
        )
        .await
        .map_err(|error| polyphony_core::Error::Adapter(error.to_string()))?;

        Ok(AgentRunResult {
            status: AttemptStatus::Succeeded,
            turns_completed: 1,
            error: None,
            final_issue_state: Some("Done".into()),
        })
    }
}

#[derive(Clone)]
struct RecordingCommitter {
    requests: Arc<Mutex<Vec<WorkspaceCommitRequest>>>,
    result: Option<WorkspaceCommitResult>,
}

impl RecordingCommitter {
    fn new(result: Option<WorkspaceCommitResult>) -> Self {
        Self {
            requests: Arc::new(Mutex::new(Vec::new())),
            result,
        }
    }

    fn requests(&self) -> Vec<WorkspaceCommitRequest> {
        self.requests.lock().unwrap().clone()
    }
}

#[async_trait]
impl WorkspaceCommitter for RecordingCommitter {
    fn component_key(&self) -> String {
        "git:test-committer".into()
    }

    async fn commit_and_push(
        &self,
        request: &WorkspaceCommitRequest,
    ) -> Result<Option<WorkspaceCommitResult>, polyphony_core::Error> {
        self.requests.lock().unwrap().push(request.clone());
        Ok(self.result.clone())
    }
}

#[derive(Clone)]
struct RecordingPullRequestManager {
    requests: Arc<Mutex<Vec<PullRequestRequest>>>,
    ensured_pull_request: PullRequestRef,
}

impl RecordingPullRequestManager {
    fn new(ensured_pull_request: PullRequestRef) -> Self {
        Self {
            requests: Arc::new(Mutex::new(Vec::new())),
            ensured_pull_request,
        }
    }

    fn requests(&self) -> Vec<PullRequestRequest> {
        self.requests.lock().unwrap().clone()
    }
}

#[async_trait]
impl PullRequestManager for RecordingPullRequestManager {
    fn component_key(&self) -> String {
        "github:test-prs".into()
    }

    async fn ensure_pull_request(
        &self,
        request: &PullRequestRequest,
    ) -> Result<PullRequestRef, polyphony_core::Error> {
        self.requests.lock().unwrap().push(request.clone());
        Ok(self.ensured_pull_request.clone())
    }

    async fn merge_pull_request(
        &self,
        _pull_request: &PullRequestRef,
    ) -> Result<(), polyphony_core::Error> {
        Ok(())
    }
}

fn test_workflow(workspace_root: &Path) -> LoadedWorkflow {
    test_workflow_with_front_matter(
        workspace_root,
        "---\ntracker:\n  kind: mock\npolling:\n  interval_ms: 1000\nworkspace:\n  root: __ROOT__\norchestration:\n  dispatch_mode: manual\nagents:\n  default: mock\n  profiles:\n    mock:\n      kind: mock\n      transport: mock\n      command: mock\n---\nTest prompt\n",
    )
}

fn pipeline_workflow_with_automation(workspace_root: &Path) -> LoadedWorkflow {
    test_workflow_with_front_matter(
        workspace_root,
        "---\ntracker:\n  kind: github\n  repository: penso/polyphony\n  api_key: test-token\n  active_states: [Todo, In Progress]\n  terminal_states: [Done]\npolling:\n  interval_ms: 1000\nworkspace:\n  root: __ROOT__\n  checkout_kind: linked_worktree\n  source_repo_path: __ROOT__/source-repo\nagent:\n  max_turns: 3\norchestration:\n  dispatch_mode: manual\n  router_agent: router\nagents:\n  default: implementer\n  profiles:\n    router:\n      kind: mock\n      transport: mock\n      command: mock\n    implementer:\n      kind: mock\n      transport: mock\n      command: mock\nautomation:\n  enabled: true\n  git:\n    remote_name: origin\n---\nFix {{ issue.identifier }}\n",
    )
}

fn test_workflow_with_front_matter(workspace_root: &Path, raw: &str) -> LoadedWorkflow {
    let workflow_path = workspace_root.join("WORKFLOW.md");
    fs::create_dir_all(workspace_root).unwrap();
    let raw = raw.replace("__ROOT__", &workspace_root.display().to_string());
    fs::write(&workflow_path, raw).unwrap();
    load_workflow(&workflow_path).unwrap()
}

fn test_service(
    tracker: TestTracker,
    provisioner: RecordingProvisioner,
    workspace_root: &Path,
) -> RuntimeService {
    let workflow = test_workflow(workspace_root);
    let (_tx, rx) = watch::channel(workflow.clone());
    RuntimeService::new(
        Arc::new(tracker),
        None,
        Arc::new(NoopAgent),
        Arc::new(provisioner),
        None,
        None,
        None,
        None,
        None,
        None,
        rx,
    )
    .0
}

fn test_service_for_workflow(
    workflow: LoadedWorkflow,
    tracker: TestTracker,
    provisioner: RecordingProvisioner,
) -> RuntimeService {
    let (_tx, rx) = watch::channel(workflow);
    RuntimeService::new(
        Arc::new(tracker),
        None,
        Arc::new(NoopAgent),
        Arc::new(provisioner),
        None,
        None,
        None,
        None,
        None,
        None,
        rx,
    )
    .0
}

fn persisted_issue_run(issue: &Issue, workspace_root: &Path, status: RunStatus) -> Run {
    let now = Utc::now();
    Run {
        id: format!("run-{}", issue.id),
        kind: RunKind::IssueDelivery,
        issue_id: Some(issue.id.clone()),
        issue_identifier: Some(issue.identifier.clone()),
        title: issue.title.clone(),
        status,
        pipeline_stage: None,
        manual_dispatch_directives: None,
        workspace_key: Some(sanitize_workspace_key(&issue.identifier)),
        workspace_path: Some(workspace_root.join(sanitize_workspace_key(&issue.identifier))),
        review_target: None,
        deliverable: None,
        created_at: now,
        updated_at: now,
        activity_log: Vec::new(),
        cancel_reason: None,
        blocked_outcome: None,
        steps: Vec::new(),
    }
}

fn test_service_with_reload(
    workflow: LoadedWorkflow,
    tracker: Arc<dyn IssueTracker>,
    agent: Arc<dyn AgentRuntime>,
    provisioner: RecordingProvisioner,
    component_factory: Arc<RuntimeComponentFactory>,
) -> RuntimeService {
    let (tx, rx) = watch::channel(workflow.clone());
    RuntimeService::new(
        tracker,
        None,
        agent,
        Arc::new(provisioner),
        None,
        None,
        None,
        None,
        None,
        None,
        rx,
    )
    .0
    .with_workflow_reload(workflow.path.clone(), None, tx, component_factory)
}

fn sample_issue(issue_id: &str, identifier: &str, state: &str, title: &str) -> Issue {
    Issue {
        id: issue_id.to_string(),
        identifier: identifier.to_string(),
        title: title.to_string(),
        description: Some(format!("Description for {title}")),
        priority: Some(1),
        state: state.to_string(),
        branch_name: Some(format!("task/{}", identifier.to_ascii_lowercase())),
        labels: vec!["test".into()],
        created_at: Some(Utc::now()),
        updated_at: Some(Utc::now()),
        ..Issue::default()
    }
}

fn sample_pull_request_comment_event() -> PullRequestCommentEvent {
    let now = Utc::now();
    PullRequestCommentEvent {
        provider: polyphony_core::ReviewProviderKind::Github,
        repository: "penso/polyphony".into(),
        number: 42,
        pull_request_title: "Review me".into(),
        url: Some("https://github.com/penso/polyphony/pull/42#discussion_r1".into()),
        base_branch: "main".into(),
        head_branch: "feature/review".into(),
        head_sha: "abc123".into(),
        checkout_ref: Some("refs/pull/42/head".into()),
        thread_id: "thread-1".into(),
        comment_id: "comment-1".into(),
        path: "crates/core/src/lib.rs".into(),
        line: Some(42),
        body: "Please fix this branch.".into(),
        author_login: Some("greptileai".into()),
        approval_state: DispatchApprovalState::Approved,
        labels: vec!["ready".into()],
        created_at: Some(now - chrono::Duration::minutes(5)),
        updated_at: Some(now - chrono::Duration::minutes(2)),
        is_draft: false,
    }
}

fn sample_pull_request_conflict_event() -> PullRequestConflictEvent {
    let now = Utc::now();
    PullRequestConflictEvent {
        provider: polyphony_core::ReviewProviderKind::Github,
        repository: "penso/polyphony".into(),
        number: 43,
        pull_request_title: "Merge me".into(),
        url: Some("https://github.com/penso/polyphony/pull/43".into()),
        base_branch: "main".into(),
        head_branch: "feature/conflict".into(),
        head_sha: "def456".into(),
        checkout_ref: Some("refs/pull/43/head".into()),
        author_login: Some("alice".into()),
        approval_state: DispatchApprovalState::Approved,
        labels: vec!["ready".into()],
        created_at: Some(now - chrono::Duration::minutes(10)),
        updated_at: Some(now - chrono::Duration::minutes(3)),
        is_draft: false,
        mergeable_state: "conflicting".into(),
        merge_state_status: "dirty".into(),
    }
}

fn make_running_task(issue: Issue, workspace_path: PathBuf) -> RunningTask {
    RunningTask {
        issue,
        agent_name: "mock".into(),
        model: None,
        attempt: None,
        workspace_path,
        stall_timeout_ms: 300_000,
        max_turns: 5,
        started_at: Utc::now(),
        session_id: None,
        thread_id: None,
        turn_id: None,
        codex_app_server_pid: None,
        last_event: None,
        last_message: None,
        last_event_at: None,
        tokens: TokenUsage::default(),
        last_reported_tokens: TokenUsage::default(),
        turn_count: 0,
        rate_limits: None,
        stop_tx: watch::channel(None).0,
        active_task_id: None,
        run_id: None,
        review_target: None,
        review_comment_marker: None,
        recent_log: VecDeque::new(),
        handle: tokio::spawn(async {
            let _: () = std::future::pending().await;
        }),
    }
}

fn unique_workspace_root(test_name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "polyphony-orchestrator-{test_name}-{}",
        Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ))
}

async fn handle_next_worker_message(service: &mut RuntimeService) {
    let message = timeout(Duration::from_secs(5), service.command_rx.recv())
        .await
        .expect("timed out waiting for orchestrator worker message")
        .expect("orchestrator command channel closed");
    service.handle_message(message).await.unwrap();
}

#[tokio::test]
async fn webhook_dispatch_uses_synthetic_issue_without_tracker_side_effects() {
    let workspace_root = unique_workspace_root("webhook-dispatch");
    let workflow = test_workflow(&workspace_root);
    let (_tx, workflow_rx) = watch::channel(workflow.clone());
    let tracker = TestTracker::new(Vec::new());
    let provisioner = RecordingProvisioner::default();
    let agent = ScriptedPipelineAgent::default();
    let (mut service, _handle) = RuntimeService::new(
        Arc::new(tracker.clone()),
        None,
        Arc::new(agent.clone()),
        Arc::new(provisioner),
        None,
        None,
        None,
        None,
        None,
        None,
        workflow_rx,
    );
    service
        .pending_webhook_dispatches
        .push(WebhookDispatchRequest {
            trigger_id: "deploy".into(),
            repo_id: None,
            issue: Issue {
                id: format!("webhook:deploy:1:{}", uuid::Uuid::new_v4()),
                identifier: "WEBHOOK-DEPLOY-1".into(),
                title: "Deploy 1".into(),
                description: Some("{\"event\":\"push\"}".into()),
                priority: None,
                state: "webhook".into(),
                branch_name: None,
                url: None,
                author: None,
                labels: vec!["webhook:deploy".into()],
                comments: Vec::new(),
                blocked_by: Vec::new(),
                approval_state: DispatchApprovalState::Approved,
                parent_id: None,
                created_at: None,
                updated_at: None,
            },
            agent_name: "mock".into(),
            model: Some("gpt-test".into()),
            prompt: "Inspect the webhook payload".into(),
        });

    service.process_pending_webhook_dispatches().await;
    handle_next_worker_message(&mut service).await;

    let calls = agent.recorded_calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "mock");
    assert_eq!(calls[0].1, "Inspect the webhook payload");
    assert!(tracker.acknowledged_issues().is_empty());
    assert!(tracker.recorded_workflow_updates().is_empty());
    assert!(
        service
            .state
            .runs
            .values()
            .any(|run| run.issue_identifier.as_deref() == Some("WEBHOOK-DEPLOY-1"))
    );
}

#[tokio::test]
async fn reconcile_running_releases_missing_issue() {
    let workspace_root = unique_workspace_root("missing");
    let provisioner = RecordingProvisioner::default();
    let mut service = test_service(TestTracker::new(Vec::new()), provisioner, &workspace_root);
    let issue = sample_issue("issue-1", "FAC-1", "Todo", "Old");
    let workspace_path = workspace_root.join("FAC-1");
    service.state.running.insert(
        issue.id.clone(),
        make_running_task(issue.clone(), workspace_path),
    );
    service.claim_issue(issue.id.clone(), IssueClaimState::Running);

    service.reconcile_running().await;

    assert!(!service.state.running.contains_key(&issue.id));
    assert!(!service.is_claimed(&issue.id));
}

#[tokio::test]
async fn reconcile_running_preserves_synthetic_pr_review_issue() {
    let workspace_root = unique_workspace_root("synthetic-pr");
    let provisioner = RecordingProvisioner::default();
    // Empty tracker — no issues will be returned by fetch_issues_by_ids.
    let mut service = test_service(TestTracker::new(Vec::new()), provisioner, &workspace_root);
    let synthetic_id = "pr_review:github:penso/arbor:89:abc123";
    let issue = Issue {
        id: synthetic_id.to_string(),
        identifier: "penso/arbor#89".into(),
        title: "Review PR #89: bump rustls-webpki".into(),
        state: "Review".into(),
        ..Issue::default()
    };
    let workspace_path = workspace_root.join("penso_arbor_89");
    service.state.running.insert(
        issue.id.clone(),
        make_running_task(issue.clone(), workspace_path),
    );
    service.claim_issue(issue.id.clone(), IssueClaimState::Running);

    service.reconcile_running().await;

    // Synthetic issue must NOT be stopped — it has no tracker-side state.
    assert!(
        service.state.running.contains_key(synthetic_id),
        "synthetic PR review issue should survive reconciliation"
    );
    assert!(service.is_claimed(synthetic_id));
}

#[tokio::test]
async fn reconcile_running_preserves_synthetic_pr_comment_issue() {
    let workspace_root = unique_workspace_root("synthetic-comment");
    let provisioner = RecordingProvisioner::default();
    let mut service = test_service(TestTracker::new(Vec::new()), provisioner, &workspace_root);
    let synthetic_id = "pr_comment:github:penso/arbor:42:thread-1";
    let issue = Issue {
        id: synthetic_id.to_string(),
        identifier: "penso/arbor#42".into(),
        title: "Comment on PR #42".into(),
        state: "Review".into(),
        ..Issue::default()
    };
    let workspace_path = workspace_root.join("penso_arbor_42");
    service.state.running.insert(
        issue.id.clone(),
        make_running_task(issue.clone(), workspace_path),
    );

    service.reconcile_running().await;

    assert!(
        service.state.running.contains_key(synthetic_id),
        "synthetic PR comment issue should survive reconciliation"
    );
}

#[tokio::test]
async fn reconcile_running_preserves_session_for_non_terminal_state() {
    let workspace_root = unique_workspace_root("non-terminal-state");
    let provisioner = RecordingProvisioner::default();
    // Tracker returns the issue with state "Open" — not in active_states ("Todo",
    // "In Progress") or terminal_states ("Done", "Closed", "Cancelled").
    // Reconciliation must NOT cancel it: only explicit terminal states stop work.
    let tracker_issue = sample_issue("issue-5", "FAC-5", "Open", "GitHub-style issue");
    let mut service = test_service(
        TestTracker::new(vec![tracker_issue.clone()]),
        provisioner,
        &workspace_root,
    );
    let running_issue = sample_issue("issue-5", "FAC-5", "Open", "GitHub-style issue");
    let workspace_path = workspace_root.join("FAC-5");
    service.state.running.insert(
        running_issue.id.clone(),
        make_running_task(running_issue.clone(), workspace_path),
    );
    service.claim_issue(running_issue.id.clone(), IssueClaimState::Running);

    service.state.runs.insert("run-5".into(), Run {
        id: "run-5".into(),
        kind: RunKind::IssueDelivery,
        issue_id: Some("issue-5".into()),
        issue_identifier: Some("FAC-5".into()),
        title: "GitHub-style issue".into(),
        status: RunStatus::InProgress,
        pipeline_stage: None,
        manual_dispatch_directives: None,
        workspace_key: None,
        workspace_path: None,
        review_target: None,
        deliverable: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        activity_log: Vec::new(),
        cancel_reason: None,
        blocked_outcome: None,
        steps: Vec::new(),
    });

    service.reconcile_running().await;

    // Session must survive — "Open" is not terminal.
    assert!(
        service.state.running.contains_key(&running_issue.id),
        "session with non-terminal state 'Open' must NOT be cancelled by reconciliation"
    );
    assert!(service.is_claimed(&running_issue.id));
    // Run must remain in progress.
    let run = service.state.runs.get("run-5").unwrap();
    assert_eq!(run.status, RunStatus::InProgress);
    assert!(
        run.cancel_reason.is_none(),
        "cancel_reason must be None for non-terminal state"
    );
}

#[tokio::test]
async fn reconcile_running_sets_cancel_reason_for_missing_issue() {
    let workspace_root = unique_workspace_root("missing-reason");
    let provisioner = RecordingProvisioner::default();
    // Empty tracker — issue will not be found.
    let mut service = test_service(TestTracker::new(Vec::new()), provisioner, &workspace_root);
    let issue = sample_issue("issue-6", "FAC-6", "Todo", "Vanished issue");
    let workspace_path = workspace_root.join("FAC-6");
    service.state.running.insert(
        issue.id.clone(),
        make_running_task(issue.clone(), workspace_path),
    );
    service.claim_issue(issue.id.clone(), IssueClaimState::Running);

    // Add a run so stop_running can set cancel_reason on it.
    service.state.runs.insert("run-6".into(), Run {
        id: "run-6".into(),
        kind: RunKind::IssueDelivery,
        issue_id: Some("issue-6".into()),
        issue_identifier: Some("FAC-6".into()),
        title: "Vanished issue".into(),
        status: RunStatus::InProgress,
        pipeline_stage: None,
        manual_dispatch_directives: None,
        workspace_key: None,
        workspace_path: None,
        review_target: None,
        deliverable: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        activity_log: Vec::new(),
        cancel_reason: None,
        blocked_outcome: None,
        steps: Vec::new(),
    });

    service.reconcile_running().await;

    assert!(!service.state.running.contains_key(&issue.id));
    let run = service.state.runs.get("run-6").unwrap();
    assert_eq!(run.status, RunStatus::Cancelled);
    assert!(
        run.cancel_reason.is_some(),
        "cancel_reason must be set for missing issues"
    );
    let reason = run.cancel_reason.as_deref().unwrap();
    assert!(
        reason.contains("no longer found"),
        "cancel_reason should explain the issue is missing, got: {reason}"
    );
}

#[tokio::test]
async fn reconcile_running_sets_cancel_reason_for_terminal_state() {
    let workspace_root = unique_workspace_root("terminal-reason");
    let provisioner = RecordingProvisioner::default();
    let tracker_issue = sample_issue("issue-7", "FAC-7", "Done", "Finished issue");
    let mut service = test_service(
        TestTracker::new(vec![tracker_issue.clone()]),
        provisioner,
        &workspace_root,
    );
    let running_issue = sample_issue("issue-7", "FAC-7", "Todo", "Finished issue");
    let workspace_path = workspace_root.join("FAC-7");
    fs::create_dir_all(&workspace_path).unwrap();
    service.state.running.insert(
        running_issue.id.clone(),
        make_running_task(running_issue.clone(), workspace_path),
    );
    service.claim_issue(running_issue.id.clone(), IssueClaimState::Running);

    service.state.runs.insert("run-7".into(), Run {
        id: "run-7".into(),
        kind: RunKind::IssueDelivery,
        issue_id: Some("issue-7".into()),
        issue_identifier: Some("FAC-7".into()),
        title: "Finished issue".into(),
        status: RunStatus::InProgress,
        pipeline_stage: None,
        manual_dispatch_directives: None,
        workspace_key: None,
        workspace_path: None,
        review_target: None,
        deliverable: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        activity_log: Vec::new(),
        cancel_reason: None,
        blocked_outcome: None,
        steps: Vec::new(),
    });

    service.reconcile_running().await;

    assert!(!service.state.running.contains_key(&running_issue.id));
    let run = service.state.runs.get("run-7").unwrap();
    assert_eq!(run.status, RunStatus::Cancelled);
    assert!(
        run.cancel_reason.is_some(),
        "cancel_reason must be set for terminal state"
    );
    let reason = run.cancel_reason.as_deref().unwrap();
    assert!(
        reason.contains("terminal"),
        "cancel_reason should mention terminal state, got: {reason}"
    );
}

#[tokio::test]
async fn tick_tracks_visible_issues_when_no_agents_are_configured() {
    let workspace_root = unique_workspace_root("visible-issues");
    let workflow = test_workflow_with_front_matter(
        &workspace_root,
        "---\ntracker:\n  kind: none\npolling:\n  interval_ms: 1000\nworkspace:\n  root: __ROOT__\nagents:\n  by_state: {}\n  by_label: {}\n  profiles: {}\n---\nTest prompt\n",
    );
    let (_tx, rx) = watch::channel(workflow.clone());
    let tracker = TestTracker::new(vec![
        sample_issue("issue-1", "FAC-1", "Todo", "First"),
        sample_issue("issue-2", "FAC-2", "In Progress", "Second"),
    ]);
    let provisioner = RecordingProvisioner::default();
    let mut service = RuntimeService::new(
        Arc::new(tracker),
        None,
        Arc::new(NoopAgent),
        Arc::new(provisioner),
        None,
        None,
        None,
        None,
        None,
        None,
        rx,
    )
    .0;

    service.tick().await;

    let snapshot = service.snapshot();
    let visible = snapshot
        .tracker_issues
        .iter()
        .map(|issue| issue.issue_identifier.as_str())
        .collect::<Vec<_>>();

    assert_eq!(visible, vec!["FAC-1", "FAC-2"]);
    assert!(snapshot.running.is_empty());
}

#[tokio::test]
async fn disappearing_issues_become_already_fixed_events() {
    let workspace_root = unique_workspace_root("discarded-issue");
    let tracker = TestTracker::new(vec![sample_issue("issue-1", "FAC-1", "Todo", "First")]);
    let tracker_handle = tracker.clone();
    let provisioner = RecordingProvisioner::default();
    let mut service = test_service(tracker, provisioner, &workspace_root);

    service.tick().await;
    tracker_handle.issues.lock().unwrap().clear();
    service.tick().await;

    let snapshot = service.snapshot();
    let discarded = snapshot
        .inbox_items
        .iter()
        .find(|item| item.item_id == "issue-1")
        .expect("missing discarded inbox item");
    assert_eq!(discarded.kind, InboxItemKind::Issue);
    assert_eq!(discarded.status, "already_fixed");
}

#[tokio::test]
async fn idle_mode_dispatches_when_budget_has_headroom() {
    let workspace_root = unique_workspace_root("idle-budget-headroom");
    let tracker = TestTracker::new(vec![sample_issue("issue-1", "FAC-1", "Todo", "First")]);
    let provisioner = RecordingProvisioner::default();
    let mut service = test_service(tracker, provisioner, &workspace_root);
    service.state.dispatch_mode = polyphony_core::DispatchMode::Idle;
    service
        .state
        .budgets
        .insert("agent:mock".into(), BudgetSnapshot {
            component: "agent:mock".into(),
            captured_at: Utc::now(),
            credits_remaining: Some(12.0),
            credits_total: Some(20.0),
            spent_usd: None,
            soft_limit_usd: None,
            hard_limit_usd: None,
            reset_at: None,
            raw: Some(json!({ "weekly_deficit": 0 })),
        });

    service.tick().await;

    assert!(service.state.running.contains_key("issue-1"));
}

#[tokio::test]
async fn idle_mode_skips_dispatch_when_weekly_budget_is_underwater() {
    let workspace_root = unique_workspace_root("idle-weekly-deficit");
    let tracker = TestTracker::new(vec![sample_issue("issue-1", "FAC-1", "Todo", "First")]);
    let provisioner = RecordingProvisioner::default();
    let mut service = test_service(tracker, provisioner, &workspace_root);
    service.state.dispatch_mode = polyphony_core::DispatchMode::Idle;
    service
        .state
        .budgets
        .insert("agent:mock".into(), BudgetSnapshot {
            component: "agent:mock".into(),
            captured_at: Utc::now(),
            credits_remaining: Some(12.0),
            credits_total: Some(20.0),
            spent_usd: None,
            soft_limit_usd: None,
            hard_limit_usd: None,
            reset_at: None,
            raw: Some(json!({ "weekly": { "deficit": 1 } })),
        });

    service.tick().await;

    assert!(!service.state.running.contains_key("issue-1"));
}

#[tokio::test]
async fn idle_mode_only_dispatches_when_no_other_work_is_running() {
    let workspace_root = unique_workspace_root("idle-busy");
    let tracker = TestTracker::new(vec![
        sample_issue("issue-1", "FAC-1", "Todo", "First"),
        sample_issue("issue-2", "FAC-2", "In Progress", "Second"),
    ]);
    let provisioner = RecordingProvisioner::default();
    let mut service = test_service(tracker, provisioner.clone(), &workspace_root);
    service.state.dispatch_mode = polyphony_core::DispatchMode::Idle;
    service
        .state
        .budgets
        .insert("agent:mock".into(), BudgetSnapshot {
            component: "agent:mock".into(),
            captured_at: Utc::now(),
            credits_remaining: Some(12.0),
            credits_total: Some(20.0),
            spent_usd: None,
            soft_limit_usd: None,
            hard_limit_usd: None,
            reset_at: None,
            raw: Some(json!({ "weekly_remaining": 3 })),
        });
    let running_issue = sample_issue("issue-2", "FAC-2", "In Progress", "Second");
    let workspace_path = workspace_root.join(sanitize_workspace_key(&running_issue.identifier));
    service.state.running.insert(
        running_issue.id.clone(),
        make_running_task(running_issue, workspace_path),
    );

    service.tick().await;

    assert!(!service.state.running.contains_key("issue-1"));
    assert!(service.state.running.contains_key("issue-2"));
}

#[tokio::test]
async fn completed_pull_request_reviews_are_marked_reviewed_and_not_redispatched() {
    let workspace_root = unique_workspace_root("pr-review");
    let workflow = test_workflow_with_front_matter(
        &workspace_root,
        "---\ntracker:\n  kind: github\n  repository: penso/polyphony\n  api_key: token\npolling:\n  interval_ms: 1000\nworkspace:\n  root: __ROOT__\nagents:\n  default: reviewer\n  profiles:\n    reviewer:\n      kind: claude\n      transport: local_cli\n      command: claude -p --verbose --dangerously-skip-permissions\nreview_events:\n  pr_reviews:\n    enabled: true\n    agent: reviewer\n    debounce_seconds: 1\n---\nPrompt\n",
    );
    let (_tx, rx) = watch::channel(workflow.clone());
    let event = PullRequestReviewEvent {
        provider: polyphony_core::ReviewProviderKind::Github,
        repository: "penso/polyphony".into(),
        number: 42,
        title: "Review me".into(),
        url: Some("https://github.com/penso/polyphony/pull/42".into()),
        base_branch: "main".into(),
        head_branch: "feature/review".into(),
        head_sha: "abc123".into(),
        checkout_ref: Some("refs/pull/42/head".into()),
        author_login: Some("alice".into()),
        approval_state: DispatchApprovalState::Approved,
        labels: vec!["ready".into()],
        created_at: Some(Utc::now() - chrono::Duration::minutes(5)),
        updated_at: Some(Utc::now() - chrono::Duration::seconds(10)),
        is_draft: false,
    };
    let commenter = RecordingPullRequestCommenter::default();
    let provisioner = RecordingProvisioner::default();
    let mut service = RuntimeService::new(
        Arc::new(TestTracker::new(Vec::new())),
        None,
        Arc::new(NoopAgent),
        Arc::new(provisioner),
        None,
        None,
        Some(Arc::new(commenter.clone())),
        None,
        None,
        None,
        rx,
    )
    .0;
    let issue = synthetic_issue_for_pull_request_review(&event);
    let workspace_path = workspace_root.join(sanitize_workspace_key(&issue.identifier));
    tokio::fs::create_dir_all(workspace_path.join(".polyphony"))
        .await
        .unwrap();
    tokio::fs::write(
        workspace_path.join(".polyphony").join("review.md"),
        "Summary\n\nReviewed penso/polyphony#42",
    )
    .await
    .unwrap();
    service.state.runs.insert("run-review".into(), Run {
        id: "run-review".into(),
        kind: RunKind::PullRequestReview,
        issue_id: Some(issue.id.clone()),
        issue_identifier: Some(issue.identifier.clone()),
        title: event.title.clone(),
        status: RunStatus::InProgress,
        pipeline_stage: None,
        manual_dispatch_directives: None,
        workspace_key: Some(sanitize_workspace_key(&issue.identifier)),
        workspace_path: Some(workspace_path.clone()),
        review_target: Some(event.review_target()),
        deliverable: None,
        created_at: Utc::now(),
        activity_log: Vec::new(),
        cancel_reason: None,
        blocked_outcome: None,
        steps: Vec::new(),
        updated_at: Utc::now(),
    });
    let running = RunningTask {
        issue: issue.clone(),
        agent_name: "reviewer".into(),
        model: None,
        attempt: None,
        workspace_path,
        stall_timeout_ms: 300_000,
        max_turns: 4,
        started_at: Utc::now(),
        session_id: None,
        thread_id: None,
        turn_id: None,
        codex_app_server_pid: None,
        last_event: None,
        last_message: None,
        last_event_at: None,
        tokens: TokenUsage::default(),
        last_reported_tokens: TokenUsage::default(),
        turn_count: 0,
        rate_limits: None,
        stop_tx: watch::channel(None).0,
        active_task_id: None,
        run_id: Some("run-review".into()),
        review_target: Some(event.review_target()),
        review_comment_marker: Some(pull_request_review_comment_marker(&event.review_target())),
        recent_log: VecDeque::new(),
        handle: tokio::spawn(async {
            let _: () = std::future::pending().await;
        }),
    };
    service
        .finish_pull_request_review(
            issue.id.clone(),
            issue.identifier.clone(),
            None,
            running,
            AgentRunResult::succeeded(1),
        )
        .await
        .unwrap();

    let comment_bodies = commenter.comment_bodies();
    assert_eq!(comment_bodies.len(), 1);
    assert!(comment_bodies[0].contains("Summary"));
    assert!(comment_bodies[0].contains("polyphony:pr-review"));
    assert!(
        service
            .state
            .reviewed_pull_request_heads
            .contains_key(&event.dedupe_key())
    );
    assert_eq!(
        service.pull_request_event_suppression(
            &service.workflow(),
            &PullRequestEvent::Review(event.clone()),
        ),
        Some(ReviewEventSuppression::AlreadyReviewed)
    );
    // Verify deliverable was created for the PR review run.
    let run = service.state.runs.get("run-review").unwrap();
    let deliverable = run
        .deliverable
        .as_ref()
        .expect("deliverable should be set for PR review");
    assert_eq!(deliverable.kind, DeliverableKind::PullRequestReview);
    assert_eq!(deliverable.status, DeliverableStatus::Reviewed);
    assert!(
        deliverable
            .description
            .as_ref()
            .unwrap()
            .contains("Summary")
    );

    tokio::fs::write(
        workspace_root
            .join(sanitize_workspace_key(&issue.identifier))
            .join(".polyphony")
            .join("review.md"),
        "Summary\n\nUpdated review body",
    )
    .await
    .unwrap();
    service
        .post_pull_request_review_comment(
            &RunningTask {
                issue,
                agent_name: "reviewer".into(),
                model: None,
                attempt: None,
                workspace_path: workspace_root
                    .join(sanitize_workspace_key(&event.display_identifier())),
                stall_timeout_ms: 300_000,
                max_turns: 4,
                started_at: Utc::now(),
                session_id: None,
                thread_id: None,
                turn_id: None,
                codex_app_server_pid: None,
                last_event: None,
                last_message: None,
                last_event_at: None,
                tokens: TokenUsage::default(),
                last_reported_tokens: TokenUsage::default(),
                turn_count: 0,
                rate_limits: None,
                stop_tx: watch::channel(None).0,
                active_task_id: None,
                run_id: Some("run-review".into()),
                review_target: Some(event.review_target()),
                review_comment_marker: Some(pull_request_review_comment_marker(
                    &event.review_target(),
                )),
                recent_log: VecDeque::new(),
                handle: tokio::spawn(async {
                    let _: () = std::future::pending().await;
                }),
            },
            &event.review_target(),
        )
        .await
        .unwrap();
    let comment_bodies = commenter.comment_bodies();
    assert_eq!(comment_bodies.len(), 1);
    assert!(comment_bodies[0].contains("Updated review body"));
}

#[test]
fn review_event_suppression_respects_authors_labels_and_bots() {
    let workspace_root = unique_workspace_root("pr-review-suppression");
    let workflow = test_workflow_with_front_matter(
        &workspace_root,
        "---\ntracker:\n  kind: github\n  repository: penso/polyphony\n  api_key: token\npolling:\n  interval_ms: 1000\nworkspace:\n  root: __ROOT__\nagents:\n  default: reviewer\n  profiles:\n    reviewer:\n      kind: claude\n      transport: local_cli\n      command: claude -p --verbose --dangerously-skip-permissions\nreview_events:\n  pr_reviews:\n    enabled: true\n    agent: reviewer\n    debounce_seconds: 1\n    only_labels: [ready]\n    ignore_labels: [wip]\n    ignore_authors: [skip-me]\n    ignore_bot_authors: true\n---\nPrompt\n",
    );
    let (_tx, rx) = watch::channel(workflow);
    let service = RuntimeService::new(
        Arc::new(TestTracker::new(Vec::new())),
        None,
        Arc::new(NoopAgent),
        Arc::new(RecordingProvisioner::default()),
        None,
        None,
        None,
        None,
        None,
        None,
        rx,
    )
    .0;
    let workflow = service.workflow();

    let base_event = PullRequestReviewEvent {
        provider: polyphony_core::ReviewProviderKind::Github,
        repository: "penso/polyphony".into(),
        number: 1,
        title: "Review me".into(),
        url: Some("https://github.com/penso/polyphony/pull/1".into()),
        base_branch: "main".into(),
        head_branch: "feature/review".into(),
        head_sha: "sha1".into(),
        checkout_ref: Some("refs/pull/1/head".into()),
        author_login: Some("skip-me".into()),
        approval_state: DispatchApprovalState::Approved,
        labels: vec!["ready".into()],
        created_at: Some(Utc::now() - chrono::Duration::minutes(5)),
        updated_at: Some(Utc::now() - chrono::Duration::seconds(10)),
        is_draft: false,
    };
    assert_eq!(
        service.pull_request_event_suppression(
            &workflow,
            &PullRequestEvent::Review(base_event.clone()),
        ),
        Some(ReviewEventSuppression::IgnoredAuthor {
            author: "skip-me".into()
        })
    );

    let bot_event = PullRequestReviewEvent {
        number: 2,
        head_sha: "sha2".into(),
        checkout_ref: Some("refs/pull/2/head".into()),
        author_login: Some("dependabot[bot]".into()),
        ..base_event.clone()
    };
    assert_eq!(
        service.pull_request_event_suppression(
            &workflow,
            &PullRequestEvent::Review(bot_event.clone()),
        ),
        Some(ReviewEventSuppression::BotAuthor {
            author: "dependabot[bot]".into()
        })
    );

    let ignored_label_event = PullRequestReviewEvent {
        number: 3,
        head_sha: "sha3".into(),
        checkout_ref: Some("refs/pull/3/head".into()),
        author_login: Some("alice".into()),
        labels: vec!["wip".into()],
        ..base_event.clone()
    };
    assert_eq!(
        service.pull_request_event_suppression(
            &workflow,
            &PullRequestEvent::Review(ignored_label_event.clone()),
        ),
        Some(ReviewEventSuppression::IgnoredLabel {
            label: "wip".into()
        })
    );

    let missing_label_event = PullRequestReviewEvent {
        number: 4,
        head_sha: "sha4".into(),
        checkout_ref: Some("refs/pull/4/head".into()),
        author_login: Some("alice".into()),
        labels: vec!["backend".into()],
        ..base_event
    };
    assert_eq!(
        service.pull_request_event_suppression(
            &workflow,
            &PullRequestEvent::Review(missing_label_event),
        ),
        Some(ReviewEventSuppression::MissingLabels {
            labels: vec!["ready".into()]
        })
    );
}

#[tokio::test]
async fn untrusted_pull_request_events_require_manual_approval() {
    let workspace_root = unique_workspace_root("pr-review-approval");
    let workflow = test_workflow_with_front_matter(
        &workspace_root,
        "---\ntracker:\n  kind: github\n  repository: penso/polyphony\n  api_key: token\npolling:\n  interval_ms: 1000\nworkspace:\n  root: __ROOT__\nagents:\n  default: reviewer\n  profiles:\n    reviewer:\n      kind: claude\n      transport: local_cli\n      command: claude -p --verbose --dangerously-skip-permissions\nreview_events:\n  pr_reviews:\n    enabled: true\n    debounce_seconds: 1\n---\nPrompt\n",
    );
    let (_tx, rx) = watch::channel(workflow);
    let mut service = RuntimeService::new(
        Arc::new(TestTracker::new(Vec::new())),
        None,
        Arc::new(NoopAgent),
        Arc::new(RecordingProvisioner::default()),
        None,
        None,
        None,
        None,
        None,
        None,
        rx,
    )
    .0;
    let event = PullRequestReviewEvent {
        provider: polyphony_core::ReviewProviderKind::Github,
        repository: "penso/polyphony".into(),
        number: 9,
        title: "Review me carefully".into(),
        url: Some("https://github.com/penso/polyphony/pull/9".into()),
        base_branch: "main".into(),
        head_branch: "feature/review".into(),
        head_sha: "sha9".into(),
        checkout_ref: Some("refs/pull/9/head".into()),
        author_login: Some("outsider".into()),
        approval_state: DispatchApprovalState::Waiting,
        labels: vec!["ready".into()],
        created_at: Some(Utc::now() - chrono::Duration::minutes(5)),
        updated_at: Some(Utc::now() - chrono::Duration::seconds(10)),
        is_draft: false,
    };
    service
        .state
        .visible_review_events
        .insert(event.dedupe_key(), event.clone());

    assert_eq!(
        service.pull_request_event_suppression(
            &service.workflow(),
            &PullRequestEvent::Review(event.clone()),
        ),
        Some(ReviewEventSuppression::AwaitingApproval)
    );

    service
        .pending_inbox_approvals
        .push((event.dedupe_key(), "github".into()));
    service.process_pending_inbox_approvals().await;

    assert_eq!(
        service.pull_request_event_approval_state(&PullRequestEvent::Review(event.clone())),
        DispatchApprovalState::Approved
    );
    let approved = service
        .snapshot()
        .inbox_items
        .into_iter()
        .find(|row| row.item_id == event.dedupe_key())
        .expect("missing inbox item after approval");
    assert_eq!(approved.approval_state, DispatchApprovalState::Approved);
}

#[test]
fn pull_request_comment_events_are_suppressed_after_a_newer_review() {
    let workspace_root = unique_workspace_root("pr-comment-suppression");
    let workflow = test_workflow_with_front_matter(
        &workspace_root,
        "---\ntracker:\n  kind: github\n  repository: penso/polyphony\n  api_key: token\npolling:\n  interval_ms: 1000\nworkspace:\n  root: __ROOT__\nagents:\n  default: reviewer\n  profiles:\n    reviewer:\n      kind: claude\n      transport: local_cli\n      command: claude -p --verbose --dangerously-skip-permissions\n---\nPrompt\n",
    );
    let (_tx, rx) = watch::channel(workflow);
    let mut service = RuntimeService::new(
        Arc::new(TestTracker::new(Vec::new())),
        None,
        Arc::new(NoopAgent),
        Arc::new(RecordingProvisioner::default()),
        None,
        None,
        None,
        None,
        None,
        None,
        rx,
    )
    .0;
    let workflow = service.workflow();
    let now = Utc::now();
    let event = PullRequestCommentEvent {
        provider: polyphony_core::ReviewProviderKind::Github,
        repository: "penso/polyphony".into(),
        number: 42,
        pull_request_title: "Review me".into(),
        url: Some("https://github.com/penso/polyphony/pull/42#discussion_r1".into()),
        base_branch: "main".into(),
        head_branch: "feature/review".into(),
        head_sha: "abc123".into(),
        checkout_ref: Some("refs/pull/42/head".into()),
        thread_id: "thread-1".into(),
        comment_id: "comment-1".into(),
        path: "crates/core/src/lib.rs".into(),
        line: Some(42),
        body: "Please fix this branch.".into(),
        author_login: Some("greptileai".into()),
        approval_state: DispatchApprovalState::Approved,
        labels: vec!["ready".into()],
        created_at: Some(now - chrono::Duration::minutes(5)),
        updated_at: Some(now - chrono::Duration::minutes(2)),
        is_draft: false,
    };
    service.state.reviewed_pull_request_heads.insert(
        review_target_key(&event.review_target()),
        ReviewedPullRequestHead {
            key: review_target_key(&event.review_target()),
            target: event.review_target(),
            reviewed_at: now - chrono::Duration::minutes(1),
            run_id: None,
        },
    );

    assert_eq!(
        service
            .pull_request_event_suppression(&workflow, &PullRequestEvent::Comment(event.clone()),),
        Some(ReviewEventSuppression::AlreadyReviewed)
    );

    service.state.reviewed_pull_request_heads.insert(
        review_target_key(&event.review_target()),
        ReviewedPullRequestHead {
            key: review_target_key(&event.review_target()),
            target: event.review_target(),
            reviewed_at: now - chrono::Duration::minutes(3),
            run_id: None,
        },
    );

    assert!(matches!(
        service.pull_request_event_suppression(
            &workflow,
            &PullRequestEvent::Comment(event),
        ),
        Some(ReviewEventSuppression::Debounced { remaining_seconds })
            if remaining_seconds > 0 && remaining_seconds <= 180
    ));
}

#[tokio::test]
async fn disappearing_pr_comment_events_become_already_fixed() {
    let workspace_root = unique_workspace_root("discarded-pr-comment");
    let workflow = test_workflow_with_front_matter(
        &workspace_root,
        "---\ntracker:\n  kind: github\n  repository: penso/polyphony\n  api_key: token\npolling:\n  interval_ms: 1000\nworkspace:\n  root: __ROOT__\nagents:\n  default: reviewer\n  profiles:\n    reviewer:\n      kind: claude\n      transport: local_cli\n      command: claude -p --verbose --dangerously-skip-permissions\nreview_events:\n  pr_reviews:\n    enabled: true\n    debounce_seconds: 1\n---\nPrompt\n",
    );
    let (_tx, rx) = watch::channel(workflow);
    let source = SequencedPullRequestEventSource::new(vec![
        vec![PullRequestEvent::Comment(
            sample_pull_request_comment_event(),
        )],
        Vec::new(),
    ]);
    let mut service = RuntimeService::new(
        Arc::new(TestTracker::new(Vec::new())),
        Some(Arc::new(source)),
        Arc::new(NoopAgent),
        Arc::new(RecordingProvisioner::default()),
        None,
        None,
        None,
        None,
        None,
        None,
        rx,
    )
    .0;
    service.state.dispatch_mode = polyphony_core::DispatchMode::Manual;

    service.tick().await;
    service.tick().await;

    let snapshot = service.snapshot();
    let discarded = snapshot
        .inbox_items
        .iter()
        .find(|item| item.kind == InboxItemKind::PullRequestComment)
        .expect("missing discarded pr comment event");
    assert_eq!(discarded.identifier, "penso/polyphony#42");
    assert_eq!(discarded.status, "already_fixed");
}

#[tokio::test]
async fn conflict_events_become_already_fixed_without_retry_churn() {
    let workspace_root = unique_workspace_root("pr-conflict-visible");
    let workflow = test_workflow_with_front_matter(
        &workspace_root,
        "---\ntracker:\n  kind: github\n  repository: penso/polyphony\n  api_key: token\npolling:\n  interval_ms: 1000\nworkspace:\n  root: __ROOT__\nagents:\n  default: reviewer\n  profiles:\n    reviewer:\n      kind: claude\n      transport: local_cli\n      command: claude -p --verbose --dangerously-skip-permissions\nreview_events:\n  pr_reviews:\n    enabled: true\n    debounce_seconds: 1\n---\nPrompt\n",
    );
    let (_tx, rx) = watch::channel(workflow);
    let source = SequencedPullRequestEventSource::new(vec![
        vec![PullRequestEvent::Conflict(
            sample_pull_request_conflict_event(),
        )],
        Vec::new(),
    ]);
    let mut service = RuntimeService::new(
        Arc::new(TestTracker::new(Vec::new())),
        Some(Arc::new(source)),
        Arc::new(NoopAgent),
        Arc::new(RecordingProvisioner::default()),
        None,
        None,
        None,
        None,
        None,
        None,
        rx,
    )
    .0;

    service.tick().await;

    let snapshot = service.snapshot();
    let conflict = snapshot
        .inbox_items
        .iter()
        .find(|item| item.kind == InboxItemKind::PullRequestConflict)
        .expect("missing conflict event");
    assert_eq!(conflict.status, "ready");
    assert!(service.state.retrying.is_empty());
    assert!(service.state.running.is_empty());

    service.tick().await;

    let snapshot = service.snapshot();
    let discarded = snapshot
        .inbox_items
        .iter()
        .find(|item| item.kind == InboxItemKind::PullRequestConflict)
        .expect("missing discarded conflict event");
    assert_eq!(discarded.status, "already_fixed");
    assert!(service.state.retrying.is_empty());
}

#[tokio::test]
async fn inline_pull_request_review_comments_are_submitted_when_requested() {
    let workspace_root = unique_workspace_root("pr-review-inline");
    let workflow = test_workflow_with_front_matter(
        &workspace_root,
        "---\ntracker:\n  kind: github\n  repository: penso/polyphony\n  api_key: token\npolling:\n  interval_ms: 1000\nworkspace:\n  root: __ROOT__\nagents:\n  default: reviewer\n  profiles:\n    reviewer:\n      kind: claude\n      transport: local_cli\n      command: claude -p --verbose --dangerously-skip-permissions\nreview_events:\n  pr_reviews:\n    enabled: true\n    agent: reviewer\n    debounce_seconds: 1\n    comment_mode: inline\n---\nPrompt\n",
    );
    let (_tx, rx) = watch::channel(workflow);
    let event = PullRequestReviewEvent {
        provider: polyphony_core::ReviewProviderKind::Github,
        repository: "penso/polyphony".into(),
        number: 42,
        title: "Review me".into(),
        url: Some("https://github.com/penso/polyphony/pull/42".into()),
        base_branch: "main".into(),
        head_branch: "feature/review".into(),
        head_sha: "abc123".into(),
        checkout_ref: Some("refs/pull/42/head".into()),
        author_login: Some("alice".into()),
        approval_state: DispatchApprovalState::Approved,
        labels: vec!["ready".into()],
        created_at: Some(Utc::now() - chrono::Duration::minutes(5)),
        updated_at: Some(Utc::now() - chrono::Duration::seconds(10)),
        is_draft: false,
    };
    let commenter = RecordingPullRequestCommenter::default();
    let mut service = RuntimeService::new(
        Arc::new(TestTracker::new(Vec::new())),
        None,
        Arc::new(NoopAgent),
        Arc::new(RecordingProvisioner::default()),
        None,
        None,
        Some(Arc::new(commenter.clone())),
        None,
        None,
        None,
        rx,
    )
    .0;
    let issue = synthetic_issue_for_pull_request_review(&event);
    let workspace_path = workspace_root.join(sanitize_workspace_key(&issue.identifier));
    tokio::fs::create_dir_all(workspace_path.join(".polyphony"))
        .await
        .unwrap();
    tokio::fs::write(
        workspace_path.join(".polyphony").join("review.md"),
        "Summary\n\nNeeds fixes",
    )
    .await
    .unwrap();
    tokio::fs::write(
        workspace_path
            .join(".polyphony")
            .join("review-comments.json"),
        r#"[{"path":"crates/core/src/lib.rs","line":42,"body":"Fix this branch."}]"#,
    )
    .await
    .unwrap();

    service
        .post_pull_request_review_comment(
            &RunningTask {
                issue,
                agent_name: "reviewer".into(),
                model: None,
                attempt: None,
                workspace_path,
                stall_timeout_ms: 300_000,
                max_turns: 4,
                started_at: Utc::now(),
                session_id: None,
                thread_id: None,
                turn_id: None,
                codex_app_server_pid: None,
                last_event: None,
                last_message: None,
                last_event_at: None,
                tokens: TokenUsage::default(),
                last_reported_tokens: TokenUsage::default(),
                turn_count: 0,
                rate_limits: None,
                stop_tx: watch::channel(None).0,
                active_task_id: None,
                run_id: Some("run-inline".into()),
                review_target: Some(event.review_target()),
                review_comment_marker: Some(pull_request_review_comment_marker(
                    &event.review_target(),
                )),
                recent_log: VecDeque::new(),
                handle: tokio::spawn(async {
                    let _: () = std::future::pending().await;
                }),
            },
            &event.review_target(),
        )
        .await
        .unwrap();

    assert!(commenter.comment_bodies().is_empty());
    let reviews = commenter.reviews();
    assert_eq!(reviews.len(), 1);
    assert_eq!(reviews[0].2.len(), 1);
    assert_eq!(reviews[0].2[0].path, "crates/core/src/lib.rs");
    assert_eq!(reviews[0].2[0].line, 42);
    assert_eq!(reviews[0].3, "abc123");
}

#[tokio::test]
async fn orphan_auto_dispatch_uses_loaded_issue_without_refetch_by_id() {
    let workspace_root = unique_workspace_root("orphan-direct-dispatch");
    let issue = sample_issue("issue-1", "FAC-1", "Todo", "First");
    let tracker = TestTracker::new(vec![issue.clone()]);
    let tracker_handle = tracker.clone();
    let provisioner = RecordingProvisioner::default();
    let mut service = test_service(tracker, provisioner, &workspace_root);
    service
        .state
        .orphan_dispatch_keys
        .insert(sanitize_workspace_key(&issue.identifier));
    let run = persisted_issue_run(&issue, &workspace_root, RunStatus::InProgress);
    service.state.runs.insert(run.id.clone(), run);
    service.state.dispatch_mode = polyphony_core::DispatchMode::Automatic;

    service.tick().await;

    assert_eq!(tracker_handle.fetch_by_ids_calls(), 0);
    assert!(service.state.running.contains_key(&issue.id));
}

#[tokio::test]
async fn orphan_recovery_never_dispatches_in_manual_mode() {
    let workspace_root = unique_workspace_root("orphan-manual-no-dispatch");
    let issue = sample_issue("issue-orphan-manual", "FAC-ORPHAN-MANUAL", "Todo", "Paused");
    let mut service = test_service(
        TestTracker::new(vec![issue.clone()]),
        RecordingProvisioner::default(),
        &workspace_root,
    );
    service
        .state
        .orphan_dispatch_keys
        .insert(sanitize_workspace_key(&issue.identifier));

    service.tick().await;

    assert_eq!(
        service.state.dispatch_mode,
        polyphony_core::DispatchMode::Manual
    );
    assert!(!service.state.running.contains_key(&issue.id));
    assert!(
        service
            .state
            .orphan_dispatch_keys
            .contains(&sanitize_workspace_key(&issue.identifier))
    );
}

#[tokio::test]
async fn orphan_recovery_never_dispatches_in_stop_mode() {
    let workspace_root = unique_workspace_root("orphan-stop-no-dispatch");
    let issue = sample_issue("issue-orphan-stop", "FAC-ORPHAN-STOP", "Todo", "Stopped");
    let mut service = test_service(
        TestTracker::new(vec![issue.clone()]),
        RecordingProvisioner::default(),
        &workspace_root,
    );
    service.state.dispatch_mode = DispatchMode::Stop;
    service
        .state
        .orphan_dispatch_keys
        .insert(sanitize_workspace_key(&issue.identifier));
    let run = persisted_issue_run(&issue, &workspace_root, RunStatus::InProgress);
    service.state.runs.insert(run.id.clone(), run);

    service.tick().await;

    assert!(!service.state.running.contains_key(&issue.id));
    assert!(
        service
            .state
            .orphan_dispatch_keys
            .contains(&sanitize_workspace_key(&issue.identifier))
    );
}

#[tokio::test]
async fn automatic_recovery_does_not_resurrect_cancelled_or_terminal_runs() {
    let workspace_root = unique_workspace_root("orphan-cancelled-no-resurrection");
    let issue = sample_issue(
        "issue-orphan-cancelled",
        "FAC-ORPHAN-CANCELLED",
        "Todo",
        "Cancelled",
    );
    let tracker = TestTracker::new(vec![issue.clone()]);
    let tracker_handle = tracker.clone();
    let mut service = test_service(tracker, RecordingProvisioner::default(), &workspace_root);
    service.state.dispatch_mode = DispatchMode::Automatic;
    service
        .state
        .orphan_dispatch_keys
        .insert(sanitize_workspace_key(&issue.identifier));
    let mut run = persisted_issue_run(&issue, &workspace_root, RunStatus::Cancelled);
    run.cancel_reason = Some("eligibility revoked: issue moved to Paused".into());
    run.push_log(
        polyphony_core::RunLogScope::Reconciliation,
        "stopped: eligibility revoked: issue moved to Paused",
    );
    service.state.runs.insert(run.id.clone(), run);

    service.tick().await;

    assert!(!service.state.running.contains_key(&issue.id));
    assert!(service.state.retrying.is_empty());
    assert!(tracker_handle.acknowledged_issues().is_empty());
    let run = service.state.runs.values().next().unwrap();
    assert_eq!(
        run.cancel_reason.as_deref(),
        Some("eligibility revoked: issue moved to Paused")
    );
    assert!(
        run.activity_log
            .iter()
            .any(|entry| entry.message.contains("eligibility revoked"))
    );
    let snapshot = service.snapshot();
    let snapshot_run = snapshot.runs.iter().find(|row| row.id == run.id).unwrap();
    assert_eq!(
        snapshot_run.cancel_reason.as_deref(),
        run.cancel_reason.as_deref()
    );

    // Terminal and failed-final runs receive the same restart safety gate.
    for status in [RunStatus::Delivered, RunStatus::Failed] {
        let mut service = test_service(
            TestTracker::new(vec![issue.clone()]),
            RecordingProvisioner::default(),
            &workspace_root,
        );
        service.state.dispatch_mode = DispatchMode::Automatic;
        service
            .state
            .orphan_dispatch_keys
            .insert(sanitize_workspace_key(&issue.identifier));
        let run = persisted_issue_run(&issue, &workspace_root, status);
        service.state.runs.insert(run.id.clone(), run);
        service.tick().await;
        assert!(
            !service.state.running.contains_key(&issue.id),
            "{status:?} run was resurrected"
        );
    }
}

#[tokio::test]
async fn equal_timestamp_persisted_run_conflict_fails_closed_on_recovery_and_retry() {
    let workspace_root = unique_workspace_root("orphan-equal-timestamp-conflict");
    let issue = sample_issue(
        "issue-orphan-equal-timestamp",
        "FAC-ORPHAN-EQUAL-TIMESTAMP",
        "Todo",
        "Equal timestamp conflict",
    );
    let tracker = TestTracker::new(vec![issue.clone()]);
    let tracker_handle = tracker.clone();
    let mut service = test_service(tracker, RecordingProvisioner::default(), &workspace_root);
    service.state.dispatch_mode = DispatchMode::Automatic;
    service
        .state
        .orphan_dispatch_keys
        .insert(sanitize_workspace_key(&issue.identifier));

    let mut in_progress = persisted_issue_run(&issue, &workspace_root, RunStatus::InProgress);
    in_progress.id = "run-equal-timestamp-in-progress".into();
    let mut cancelled = persisted_issue_run(&issue, &workspace_root, RunStatus::Cancelled);
    cancelled.id = "run-equal-timestamp-cancelled".into();
    cancelled.cancel_reason = Some("eligibility revoked".into());
    // These intentionally tie, as can happen with low-resolution persistence
    // timestamps. Recovery must not let HashMap iteration select the active one.
    cancelled.updated_at = in_progress.updated_at;
    service
        .state
        .runs
        .insert(in_progress.id.clone(), in_progress);
    service.state.runs.insert(cancelled.id.clone(), cancelled);
    service.schedule_retry(
        issue.id.clone(),
        issue.identifier.clone(),
        1,
        Some("stale retry".into()),
        true,
        60_000,
    );
    service.state.retrying.get_mut(&issue.id).unwrap().due_at = Instant::now();

    // The retry entry point and the orphan-recovery entry point must both
    // refuse the ambiguous persisted lifecycle.
    service.process_due_retries().await;
    service.tick().await;

    assert!(service.has_non_resumable_persisted_run(&issue.id));
    assert!(!service.has_resumable_persisted_run(&issue.id));
    assert!(service.state.retrying.is_empty(), "no retry may survive");
    assert!(
        !service.state.running.contains_key(&issue.id),
        "no worker may be started from ambiguous persisted runs"
    );
    assert!(
        tracker_handle.acknowledged_issues().is_empty(),
        "no dispatch acknowledgement may be emitted"
    );
}

#[tokio::test]
async fn automatic_recovery_skips_ineligible_persisted_run_when_configured_to_stop() {
    let workspace_root = unique_workspace_root("orphan-ineligible-no-resurrection");
    let workflow = test_workflow_with_front_matter(
        &workspace_root,
        "---\ntracker:\n  kind: mock\n  active_states: [Ready]\n  terminal_states: [Done]\n  stop_when_ineligible: true\nworkspace:\n  root: __ROOT__\norchestration:\n  dispatch_mode: automatic\nagents:\n  default: mock\n  profiles:\n    mock:\n      kind: mock\n      transport: mock\n      command: mock\n---\nTest prompt\n",
    );
    let issue = sample_issue(
        "issue-orphan-ineligible",
        "FAC-ORPHAN-INELIGIBLE",
        "Todo",
        "Ineligible",
    );
    let mut service = test_service_for_workflow(
        workflow,
        TestTracker::new(vec![issue.clone()]),
        RecordingProvisioner::default(),
    );
    service.state.dispatch_mode = DispatchMode::Automatic;
    service
        .state
        .orphan_dispatch_keys
        .insert(sanitize_workspace_key(&issue.identifier));
    let run = persisted_issue_run(&issue, &workspace_root, RunStatus::InProgress);
    service.state.runs.insert(run.id.clone(), run);

    service.tick().await;

    assert!(!service.state.running.contains_key(&issue.id));
    assert!(service.state.retrying.is_empty());
}

#[tokio::test]
async fn retry_queue_cannot_bypass_cancelled_run_recovery_gate() {
    let workspace_root = unique_workspace_root("retry-cancelled-no-resurrection");
    let issue = sample_issue(
        "issue-retry-cancelled",
        "FAC-RETRY-CANCELLED",
        "Todo",
        "Cancelled",
    );
    let tracker = TestTracker::new(vec![issue.clone()]);
    let tracker_handle = tracker.clone();
    let mut service = test_service(tracker, RecordingProvisioner::default(), &workspace_root);
    service.state.dispatch_mode = DispatchMode::Automatic;
    let mut run = persisted_issue_run(&issue, &workspace_root, RunStatus::Cancelled);
    run.cancel_reason = Some("cancelled by user".into());
    service.state.runs.insert(run.id.clone(), run);
    service.schedule_retry(
        issue.id.clone(),
        issue.identifier.clone(),
        1,
        Some("stale persisted retry".into()),
        true,
        60_000,
    );
    service.state.retrying.get_mut(&issue.id).unwrap().due_at = Instant::now();

    service.process_due_retries().await;

    assert!(service.state.retrying.is_empty());
    assert!(!service.is_claimed(&issue.id));
    assert!(!service.state.running.contains_key(&issue.id));
    assert!(tracker_handle.acknowledged_issues().is_empty());
}

#[test]
fn restart_preserves_cancellation_history_and_drops_stale_retry() {
    let workspace_root = unique_workspace_root("restart-cancellation-history");
    let issue = sample_issue(
        "issue-restart-cancelled",
        "FAC-RESTART-CANCELLED",
        "Todo",
        "Cancelled",
    );
    let mut service = test_service(
        TestTracker::new(vec![issue.clone()]),
        RecordingProvisioner::default(),
        &workspace_root,
    );
    let mut run = persisted_issue_run(&issue, &workspace_root, RunStatus::Cancelled);
    run.cancel_reason = Some("cancelled by reconciliation after eligibility revocation".into());
    run.push_log(
        polyphony_core::RunLogScope::Reconciliation,
        "stopped: cancelled by reconciliation after eligibility revocation",
    );
    let run_id = run.id.clone();
    let retry = RetryRow {
        repo_id: String::new(),
        issue_id: issue.id.clone(),
        issue_identifier: issue.identifier.clone(),
        attempt: 2,
        due_at: Utc::now(),
        error: Some("stale retry from before cancellation".into()),
    };

    service.restore_bootstrap(StoreBootstrap {
        snapshot: None,
        retrying: HashMap::from([(issue.id.clone(), retry)]),
        throttles: HashMap::new(),
        budgets: HashMap::new(),
        saved_contexts: HashMap::new(),
        recent_events: Vec::new(),
        runs: HashMap::from([(run_id.clone(), run)]),
        tasks: HashMap::new(),
        reviewed_pull_request_heads: HashMap::new(),
        agent_run_history: Vec::new(),
    });

    assert!(service.state.retrying.is_empty());
    assert!(!service.is_claimed(&issue.id));
    let restored = service.state.runs.get(&run_id).unwrap();
    assert_eq!(
        restored.cancel_reason.as_deref(),
        Some("cancelled by reconciliation after eligibility revocation")
    );
    assert!(
        restored
            .activity_log
            .iter()
            .any(|entry| entry.message.contains("eligibility revocation"))
    );
}

#[tokio::test]
async fn first_tick_shows_issues_before_startup_cleanup_finishes() {
    let workspace_root = unique_workspace_root("startup-first-paint");
    let issue = sample_issue("issue-startup-1", "FAC-STARTUP-1", "Todo", "First paint");
    let tracker = DelayedCleanupTracker {
        issues: Arc::new(vec![issue.clone()]),
        cleanup_gate: Arc::new(Notify::new()),
    };
    let workflow = test_workflow(&workspace_root);
    let (_tx, workflow_rx) = watch::channel(workflow);
    let (service, handle) = RuntimeService::new(
        Arc::new(tracker.clone()),
        None,
        Arc::new(NoopAgent),
        Arc::new(RecordingProvisioner::default()),
        None,
        None,
        None,
        None,
        None,
        None,
        workflow_rx,
    );
    let mut snapshot_rx = handle.snapshot_rx.clone();
    let command_tx = handle.command_tx.clone();
    let service_task = tokio::spawn(async move { service.run().await });

    timeout(Duration::from_secs(1), async {
        loop {
            let snapshot = snapshot_rx.borrow().clone();
            if snapshot
                .tracker_issues
                .iter()
                .any(|row| row.issue_id == issue.id)
            {
                break;
            }
            snapshot_rx
                .changed()
                .await
                .expect("snapshot channel closed");
        }
    })
    .await
    .expect("first issue snapshot should not wait for startup cleanup");

    tracker.cleanup_gate.notify_waiters();
    let _ = command_tx.send(RuntimeCommand::Shutdown);
    service_task.await.unwrap().unwrap();
}

#[tokio::test]
async fn run_preserves_restored_cancelled_run_before_first_snapshot() {
    let workspace_root = unique_workspace_root("startup-normalize-stale-run");
    let tracker = TestTracker::new(Vec::new());
    let workflow = test_workflow(&workspace_root);
    let (_tx, workflow_rx) = watch::channel(workflow);
    let store = Arc::new(polyphony_core::file_store::JsonStateStore::new(
        workspace_root.join("state.json"),
    ));
    let now = Utc::now();
    let run = Run {
        id: "run-startup-stale".into(),
        kind: RunKind::PullRequestReview,
        issue_id: Some("issue-89".into()),
        issue_identifier: Some("penso/arbor#89".into()),
        title: "Review PR".into(),
        status: RunStatus::Cancelled,
        pipeline_stage: None,
        manual_dispatch_directives: None,
        workspace_key: Some("penso_arbor_89".into()),
        workspace_path: Some(workspace_root.join("penso_arbor_89")),
        review_target: None,
        deliverable: None,
        created_at: now,
        activity_log: Vec::new(),
        cancel_reason: Some("stopped by user".into()),
        blocked_outcome: None,
        steps: Vec::new(),
        updated_at: now,
    };
    let task = Task {
        id: "task-startup-stale".into(),
        run_id: run.id.clone(),
        title: "Run PR review".into(),
        description: None,
        activity_log: Vec::new(),
        category: polyphony_core::TaskCategory::Review,
        role: polyphony_core::PipelineTaskRole::Implementation,
        status: TaskStatus::InProgress,
        ordinal: 1,
        parent_id: None,
        agent_name: Some("reviewer".into()),
        session_id: None,
        thread_id: None,
        turns_completed: 0,
        tokens: TokenUsage::default(),
        started_at: Some(now),
        finished_at: None,
        error: None,
        created_at: now,
        updated_at: now,
    };
    polyphony_core::StateStore::save_run(store.as_ref(), &run)
        .await
        .unwrap();
    polyphony_core::StateStore::save_task(store.as_ref(), &task)
        .await
        .unwrap();

    let (service, handle) = RuntimeService::new(
        Arc::new(tracker),
        None,
        Arc::new(NoopAgent),
        Arc::new(RecordingProvisioner::default()),
        None,
        None,
        None,
        None,
        Some(store),
        None,
        workflow_rx,
    );
    let mut snapshot_rx = handle.snapshot_rx.clone();
    let command_tx = handle.command_tx.clone();
    let service_task = tokio::spawn(async move { service.run().await });

    let snapshot = timeout(Duration::from_secs(2), async {
        loop {
            let snapshot = snapshot_rx.borrow().clone();
            if snapshot
                .runs
                .iter()
                .any(|row| row.id == run.id && row.status == RunStatus::Cancelled)
            {
                break snapshot;
            }
            snapshot_rx
                .changed()
                .await
                .expect("snapshot channel closed");
        }
    })
    .await
    .expect("startup snapshot should include preserved cancelled run");

    let run_row = snapshot
        .runs
        .iter()
        .find(|row| row.id == run.id)
        .expect("run row");
    assert_eq!(run_row.status, RunStatus::Cancelled);
    assert_eq!(run_row.cancel_reason.as_deref(), Some("stopped by user"));
    let task_row = snapshot
        .tasks
        .iter()
        .find(|row| row.id == task.id)
        .expect("task row");
    assert_eq!(task_row.status, TaskStatus::Cancelled);
    assert_eq!(task_row.error.as_deref(), Some("stopped by user"));

    let _ = command_tx.send(RuntimeCommand::Shutdown);
    service_task.await.unwrap().unwrap();
}

#[tokio::test]
async fn automatic_dispatch_skips_waiting_issue_approval() {
    let workspace_root = unique_workspace_root("approval-waiting");
    let mut issue = sample_issue("issue-approval-1", "FAC-APPROVAL-1", "Todo", "Review input");
    issue.approval_state = polyphony_core::DispatchApprovalState::Waiting;
    let tracker = TestTracker::new(vec![issue.clone()]);
    let provisioner = RecordingProvisioner::default();
    let mut service = test_service(tracker, provisioner, &workspace_root);
    service.state.dispatch_mode = polyphony_core::DispatchMode::Automatic;

    service.tick().await;

    assert!(!service.state.running.contains_key(&issue.id));
    assert_eq!(service.state.tracker_issues.len(), 1);
    assert_eq!(
        service.state.tracker_issues[0].approval_state,
        polyphony_core::DispatchApprovalState::Waiting
    );
}

#[tokio::test]
async fn manual_dispatch_still_runs_waiting_issue_without_approval_override() {
    let workspace_root = unique_workspace_root("approval-manual-dispatch");
    let mut issue = sample_issue("issue-approval-2", "FAC-APPROVAL-2", "Todo", "Manual only");
    issue.approval_state = polyphony_core::DispatchApprovalState::Waiting;
    let tracker = TestTracker::new(vec![issue.clone()]);
    let mut service = test_service(tracker, RecordingProvisioner::default(), &workspace_root);
    service
        .pending_manual_dispatches
        .push(crate::ManualDispatchRequest {
            issue_id: issue.id.clone(),
            agent_name: None,
            directives: None,
        });

    service.process_manual_dispatches().await;

    assert!(service.state.running.contains_key(&issue.id));
    assert!(service.state.approved_inbox_keys.is_empty());
}

#[tokio::test]
async fn approving_waiting_issue_persists_and_allows_automatic_dispatch() {
    let workspace_root = unique_workspace_root("approval-approved");
    let mut issue = sample_issue("issue-approval-3", "FAC-APPROVAL-3", "Todo", "Approve me");
    issue.approval_state = polyphony_core::DispatchApprovalState::Waiting;
    let tracker = TestTracker::new(vec![issue.clone()]);
    let mut service = test_service(tracker, RecordingProvisioner::default(), &workspace_root);
    service.state.dispatch_mode = polyphony_core::DispatchMode::Automatic;

    service.tick().await;
    service
        .pending_inbox_approvals
        .push((issue.id.clone(), "mock".into()));
    service.process_pending_inbox_approvals().await;

    let snapshot = service.snapshot();
    assert_eq!(snapshot.approved_inbox_keys, vec!["mock:issue-approval-3"]);
    assert_eq!(snapshot.inbox_items.len(), 1);
    assert_eq!(
        snapshot.inbox_items[0].approval_state,
        polyphony_core::DispatchApprovalState::Approved
    );

    service.tick().await;

    assert!(service.state.running.contains_key(&issue.id));
}

#[tokio::test]
async fn closing_visible_issue_updates_tracker_and_cleans_workspace() {
    let workspace_root = unique_workspace_root("close-issue-trigger");
    let issue = sample_issue("issue-close-1", "FAC-CLOSE-1", "Todo", "Already done");
    let tracker = TestTracker::new(vec![issue.clone()]);
    let tracker_for_assertions = tracker.clone();
    let provisioner = RecordingProvisioner::default();
    let provisioner_for_assertions = provisioner.clone();
    let mut service = test_service(tracker, provisioner, &workspace_root);

    service.tick().await;

    let workspace_key = sanitize_workspace_key(&issue.identifier);
    service.state.worktree_keys.insert(workspace_key.clone());
    tokio::fs::create_dir_all(workspace_root.join(&workspace_key))
        .await
        .expect("workspace directory created");

    service.pending_issue_closures.push(issue.id.clone());
    service.process_pending_issue_closures().await;

    assert!(
        service
            .state
            .tracker_issues
            .iter()
            .all(|row| row.issue_id != issue.id),
        "closed issue should be removed from active issue rows"
    );
    assert_eq!(
        tracker_for_assertions
            .issues
            .lock()
            .unwrap()
            .get(&issue.id)
            .expect("issue exists")
            .state,
        "Closed"
    );
    assert_eq!(
        tracker_for_assertions.recorded_issue_updates().len(),
        1,
        "tracker should receive one close update"
    );
    assert_eq!(
        provisioner_for_assertions.cleaned_issue_identifiers(),
        vec![issue.identifier.clone()],
    );
    assert!(
        !service.state.worktree_keys.contains(&workspace_key),
        "workspace key should be removed after cleanup"
    );
}

#[tokio::test]
async fn reconcile_running_cleans_workspace_for_terminal_issue() {
    let workspace_root = unique_workspace_root("terminal");
    let provisioner = RecordingProvisioner::default();
    let tracker_issue = sample_issue("issue-2", "FAC-2", "Done", "Closed");
    let mut service = test_service(
        TestTracker::new(vec![tracker_issue.clone()]),
        provisioner.clone(),
        &workspace_root,
    );
    let running_issue = sample_issue("issue-2", "FAC-2", "Todo", "Open");
    let workspace_path = workspace_root.join("FAC-2");
    fs::create_dir_all(&workspace_path).unwrap();
    service.state.running.insert(
        running_issue.id.clone(),
        make_running_task(running_issue.clone(), workspace_path),
    );
    service.claim_issue(running_issue.id.clone(), IssueClaimState::Running);

    service.reconcile_running().await;

    assert!(!service.state.running.contains_key(&running_issue.id));
    assert_eq!(provisioner.cleaned_issue_identifiers(), vec![
        running_issue.identifier
    ]);
}

#[tokio::test]
async fn reconcile_running_replaces_full_issue_snapshot() {
    let workspace_root = unique_workspace_root("refresh");
    let provisioner = RecordingProvisioner::default();
    let mut refreshed_issue = sample_issue("issue-3", "FAC-3", "Todo", "Updated title");
    refreshed_issue.author = Some(IssueAuthor {
        id: Some("author-1".into()),
        username: Some("outsider".into()),
        display_name: Some("Outsider".into()),
        role: Some("none".into()),
        trust_level: Some("outsider".into()),
        url: None,
    });
    refreshed_issue.comments.push(IssueComment {
        id: "comment-1".into(),
        body: "New follow-up context".into(),
        author: refreshed_issue.author.clone(),
        url: None,
        created_at: Some(Utc::now()),
        updated_at: Some(Utc::now()),
    });
    let mut service = test_service(
        TestTracker::new(vec![refreshed_issue.clone()]),
        provisioner,
        &workspace_root,
    );
    let stale_issue = sample_issue("issue-3", "FAC-3", "Todo", "Old title");
    let workspace_path = workspace_root.join("FAC-3");
    service.state.running.insert(
        stale_issue.id.clone(),
        make_running_task(stale_issue.clone(), workspace_path),
    );
    service.claim_issue(stale_issue.id.clone(), IssueClaimState::Running);

    service.reconcile_running().await;

    let running = service.state.running.get(&stale_issue.id).unwrap();
    assert_eq!(running.issue.title, "Updated title");
    assert_eq!(running.issue.comments.len(), 1);
    assert_eq!(
        running
            .issue
            .author
            .as_ref()
            .and_then(|author| author.trust_level.as_deref()),
        Some("outsider")
    );
}

#[tokio::test]
async fn finish_running_success_marks_completed_and_queues_retry() {
    let workspace_root = unique_workspace_root("finish");
    let provisioner = RecordingProvisioner::default();
    let mut service = test_service(TestTracker::new(Vec::new()), provisioner, &workspace_root);
    let issue = sample_issue("issue-4", "FAC-4", "Todo", "Work");
    let workspace_path = workspace_root.join("FAC-4");
    service.state.running.insert(
        issue.id.clone(),
        make_running_task(issue.clone(), workspace_path),
    );
    service.claim_issue(issue.id.clone(), IssueClaimState::Running);

    service
        .finish_running(
            issue.id.clone(),
            issue.identifier.clone(),
            None,
            Utc::now(),
            AgentRunResult {
                status: AttemptStatus::Succeeded,
                turns_completed: 1,
                error: None,
                final_issue_state: Some("Human Review".into()),
            },
        )
        .await
        .unwrap();

    assert!(service.state.completed.contains(&issue.id));
    assert!(service.state.retrying.contains_key(&issue.id));
    assert_eq!(
        service.state.claim_states.get(&issue.id),
        Some(&IssueClaimState::RetryQueued)
    );
}

#[tokio::test]
async fn finish_running_with_active_final_state_skips_workflow_transition() {
    let workspace_root = unique_workspace_root("finish-active");
    let provisioner = RecordingProvisioner::default();
    let tracker = TestTracker::new(Vec::new());
    let mut service = test_service(tracker.clone(), provisioner, &workspace_root);
    let issue = sample_issue("issue-4b", "FAC-4B", "Todo", "Work");
    let workspace_path = workspace_root.join("FAC-4B");
    service.state.running.insert(
        issue.id.clone(),
        make_running_task(issue.clone(), workspace_path),
    );
    service.claim_issue(issue.id.clone(), IssueClaimState::Running);

    service
        .finish_running(
            issue.id.clone(),
            issue.identifier.clone(),
            None,
            Utc::now(),
            AgentRunResult {
                status: AttemptStatus::Succeeded,
                turns_completed: 2,
                error: None,
                final_issue_state: Some("Todo".into()),
            },
        )
        .await
        .unwrap();

    assert!(tracker.recorded_workflow_updates().is_empty());
    assert!(service.state.retrying.contains_key(&issue.id));
}

#[tokio::test]
async fn blocked_outcome_is_durable_and_prevents_retry_dispatch_and_restart() {
    let workspace_root = unique_workspace_root("blocked-outcome");
    let workflow = test_workflow_with_front_matter(
        &workspace_root,
        "---\ntracker:\n  kind: mock\n  active_states: [Todo, Awaiting Dependency]\n  blocked_state: Awaiting Dependency\nworkspace:\n  root: __ROOT__\norchestration:\n  dispatch_mode: automatic\nagents:\n  default: mock\n  profiles:\n    mock: { kind: mock, transport: mock, command: mock }\n---\nTest prompt\n",
    );
    let issue = sample_issue("issue-blocked", "FAC-BLOCKED", "Todo", "Blocked work");
    let tracker = TestTracker::new(vec![issue.clone()]);
    let tracker_handle = tracker.clone();
    let mut service = test_service_for_workflow(
        workflow.clone(),
        tracker,
        RecordingProvisioner::default(),
    );
    let run = persisted_issue_run(&issue, &workspace_root, RunStatus::InProgress);
    let run_id = run.id.clone();
    service.state.runs.insert(run_id.clone(), run);
    service.state.running.insert(
        issue.id.clone(),
        make_running_task(issue.clone(), workspace_root.join("FAC-BLOCKED")),
    );
    service.claim_issue(issue.id.clone(), IssueClaimState::Running);

    service
        .finish_running(
            issue.id.clone(),
            issue.identifier.clone(),
            None,
            Utc::now(),
            AgentRunResult {
                status: AttemptStatus::Succeeded,
                turns_completed: 1,
                error: None,
                final_issue_state: Some(
                    "BLOCKED:\nreason: waiting for an API contract\nevidence: endpoint returned 404 in the disposable fixture\nprerequisite: FAC-42"
                        .into(),
                ),
            },
        )
        .await
        .unwrap();

    let run = &service.state.runs[&run_id];
    assert_eq!(run.status, RunStatus::Blocked);
    assert_eq!(
        run.blocked_outcome.as_ref().map(|outcome| outcome.prerequisite.as_str()),
        Some("FAC-42")
    );
    assert_eq!(tracker_handle.recorded_workflow_updates(), vec!["Awaiting Dependency"]);
    assert_eq!(tracker_handle.recorded_comments().len(), 1);
    assert_eq!(
        tracker_handle.write_order(),
        vec!["comment", "workflow:Awaiting Dependency"]
    );
    assert!(tracker_handle.recorded_comments()[0].body.contains("FAC-42"));
    assert!(service.state.retrying.is_empty());
    assert!(!service.should_dispatch(&workflow, &issue));

    service.retry_failed_run_from_task(&run_id, None).await.unwrap();
    service
        .dispatch_issue(workflow.clone(), issue.clone(), None, false, None, false, None)
        .await
        .unwrap();
    assert!(!service.state.running.contains_key(&issue.id));
    let error = service
        .inject_feedback_task(&FeedbackInjectionRequest {
            run_id: run_id.clone(),
            prompt: "try to continue blocked work".into(),
            agent_name: None,
        })
        .await
        .unwrap_err();
    assert!(error.to_string().contains("blocked"));

    // Persist the run, construct a new service, and restore the durable
    // bootstrap. Automatic poll ticks must leave the terminal block intact
    // and must not acknowledge, dispatch, or add tracker evidence again.
    let persisted_bootstrap: StoreBootstrap = serde_json::from_value(
        serde_json::to_value(StoreBootstrap {
            snapshot: None,
            retrying: std::collections::HashMap::new(),
            throttles: std::collections::HashMap::new(),
            budgets: std::collections::HashMap::new(),
            saved_contexts: std::collections::HashMap::new(),
            recent_events: Vec::new(),
            runs: std::collections::HashMap::from([(
                run_id.clone(),
                service.state.runs[&run_id].clone(),
            )]),
            tasks: std::collections::HashMap::new(),
            reviewed_pull_request_heads: std::collections::HashMap::new(),
            agent_run_history: Vec::new(),
        })
        .unwrap(),
    )
    .unwrap();
    let mut restarted = test_service_for_workflow(
        workflow.clone(),
        tracker_handle.clone(),
        RecordingProvisioner::default(),
    );
    restarted.restore_bootstrap(persisted_bootstrap);
    restarted.normalize_restored_in_progress_runs().await.unwrap();
    restarted.tick().await;
    restarted.tick().await;

    assert_eq!(restarted.state.runs[&run_id].status, RunStatus::Blocked);
    assert_eq!(
        restarted.state.runs[&run_id]
            .blocked_outcome
            .as_ref()
            .map(|outcome| outcome.prerequisite.as_str()),
        Some("FAC-42")
    );
    assert!(!restarted.should_dispatch(&workflow, &issue));
    assert!(restarted.state.running.is_empty());
    assert!(restarted.state.retrying.is_empty());
    assert!(tracker_handle.acknowledged_issues().is_empty());
    assert_eq!(tracker_handle.recorded_comments().len(), 1);
    assert_eq!(tracker_handle.recorded_workflow_updates(), vec!["Awaiting Dependency"]);
}

#[tokio::test]
async fn malformed_or_unconfigured_blocked_outcomes_never_create_a_false_block() {
    let workspace_root = unique_workspace_root("blocked-outcome-rejection");
    let issue = sample_issue("issue-blocked-rejected", "FAC-REJECT", "Todo", "Rejected block");

    // Missing configuration fails closed before any tracker evidence or local
    // terminal record is accepted.
    let tracker = TestTracker::new(vec![issue.clone()]);
    let tracker_handle = tracker.clone();
    let mut service = test_service(tracker, RecordingProvisioner::default(), &workspace_root);
    let run = persisted_issue_run(&issue, &workspace_root, RunStatus::InProgress);
    let run_id = run.id.clone();
    service.state.runs.insert(run_id.clone(), run);
    service.state.running.insert(
        issue.id.clone(),
        make_running_task(issue.clone(), workspace_root.join("FAC-REJECT")),
    );
    service
        .finish_running(
            issue.id.clone(),
            issue.identifier.clone(),
            None,
            Utc::now(),
            AgentRunResult {
                status: AttemptStatus::Succeeded,
                turns_completed: 1,
                error: None,
                final_issue_state: Some(
                    "BLOCKED:\nreason: dependency missing\nevidence: fixture failed\nprerequisite: FAC-42".into(),
                ),
            },
        )
        .await
        .unwrap();
    assert_ne!(service.state.runs[&run_id].status, RunStatus::Blocked);
    assert!(service.state.runs[&run_id].blocked_outcome.is_none());
    assert!(tracker_handle.recorded_comments().is_empty());

    // An incomplete record is rejected before it can write tracker evidence.
    let workflow = test_workflow_with_front_matter(
        &workspace_root,
        "---\ntracker:\n  kind: mock\n  active_states: [Todo, Awaiting Dependency]\n  blocked_state: Awaiting Dependency\nworkspace:\n  root: __ROOT__\nagents:\n  default: mock\n  profiles:\n    mock: { kind: mock, transport: mock, command: mock }\n---\nTest prompt\n",
    );
    let tracker = TestTracker::new(vec![issue.clone()]);
    let tracker_handle = tracker.clone();
    let mut service =
        test_service_for_workflow(workflow, tracker, RecordingProvisioner::default());
    let run = persisted_issue_run(&issue, &workspace_root, RunStatus::InProgress);
    let run_id = run.id.clone();
    service.state.runs.insert(run_id.clone(), run);
    service.state.running.insert(
        issue.id.clone(),
        make_running_task(issue.clone(), workspace_root.join("FAC-REJECT-2")),
    );
    service
        .finish_running(
            issue.id.clone(),
            issue.identifier.clone(),
            None,
            Utc::now(),
            AgentRunResult {
                status: AttemptStatus::Succeeded,
                turns_completed: 1,
                error: None,
                final_issue_state: Some("BLOCKED:\nreason: dependency missing\nevidence: fixture failed".into()),
            },
        )
        .await
        .unwrap();
    assert_ne!(service.state.runs[&run_id].status, RunStatus::Blocked);
    assert!(tracker_handle.recorded_comments().is_empty());
}

#[tokio::test]
async fn invalid_prerequisite_reference_never_creates_a_false_block() {
    let workspace_root = unique_workspace_root("blocked-outcome-invalid-prerequisite");
    let workflow = test_workflow_with_front_matter(
        &workspace_root,
        "---\ntracker:\n  kind: mock\n  active_states: [Todo, Awaiting Dependency]\n  blocked_state: Awaiting Dependency\nworkspace:\n  root: __ROOT__\nagents:\n  default: mock\n  profiles:\n    mock: { kind: mock, transport: mock, command: mock }\n---\nTest prompt\n",
    );
    let issue = sample_issue(
        "issue-blocked-invalid-prerequisite",
        "FAC-INVALID-PREREQUISITE",
        "Todo",
        "Invalid prerequisite",
    );
    let tracker = TestTracker::new(vec![issue.clone()]);
    let tracker_handle = tracker.clone();
    let mut service =
        test_service_for_workflow(workflow, tracker, RecordingProvisioner::default());
    let run = persisted_issue_run(&issue, &workspace_root, RunStatus::InProgress);
    let run_id = run.id.clone();
    service.state.runs.insert(run_id.clone(), run);
    service.state.running.insert(
        issue.id.clone(),
        make_running_task(
            issue.clone(),
            workspace_root.join("FAC-INVALID-PREREQUISITE"),
        ),
    );

    service
        .finish_running(
            issue.id.clone(),
            issue.identifier.clone(),
            None,
            Utc::now(),
            AgentRunResult {
                status: AttemptStatus::Succeeded,
                turns_completed: 1,
                error: None,
                final_issue_state: Some(
                    "BLOCKED:\nreason: dependency missing\nevidence: fixture failed\nprerequisite: arbitrary text"
                        .into(),
                ),
            },
        )
        .await
        .unwrap();

    assert_ne!(service.state.runs[&run_id].status, RunStatus::Blocked);
    assert!(service.state.runs[&run_id].blocked_outcome.is_none());
    assert!(tracker_handle.recorded_comments().is_empty());
    assert!(tracker_handle.recorded_workflow_updates().is_empty());
}

#[tokio::test]
async fn tracker_write_failure_leaves_the_run_non_blocked() {
    let workspace_root = unique_workspace_root("blocked-outcome-tracker-failure");
    let workflow = test_workflow_with_front_matter(
        &workspace_root,
        "---\ntracker:\n  kind: mock\n  active_states: [Todo, Awaiting Dependency]\n  blocked_state: Awaiting Dependency\nworkspace:\n  root: __ROOT__\nagents:\n  default: mock\n  profiles:\n    mock: { kind: mock, transport: mock, command: mock }\n---\nTest prompt\n",
    );
    let issue = sample_issue("issue-blocked-write", "FAC-WRITE", "Todo", "Write failure");
    let tracker = TestTracker::new(vec![issue.clone()]).fail_workflow_status_updates("tracker unavailable");
    let tracker_handle = tracker.clone();
    let mut service = test_service_for_workflow(workflow, tracker, RecordingProvisioner::default());
    let run = persisted_issue_run(&issue, &workspace_root, RunStatus::InProgress);
    let run_id = run.id.clone();
    service.state.runs.insert(run_id.clone(), run);
    service.state.running.insert(
        issue.id.clone(),
        make_running_task(issue.clone(), workspace_root.join("FAC-WRITE")),
    );

    service
        .finish_running(
            issue.id.clone(),
            issue.identifier.clone(),
            None,
            Utc::now(),
            AgentRunResult {
                status: AttemptStatus::Succeeded,
                turns_completed: 1,
                error: None,
                final_issue_state: Some(
                    "BLOCKED:\nreason: dependency missing\nevidence: fixture failed\nprerequisite: FAC-42".into(),
                ),
            },
        )
        .await
        .unwrap();

    assert_ne!(service.state.runs[&run_id].status, RunStatus::Blocked);
    assert!(service.state.runs[&run_id].blocked_outcome.is_none());
    assert_eq!(tracker_handle.recorded_comments().len(), 1);
    assert!(tracker_handle.recorded_workflow_updates().is_empty());
    assert_eq!(tracker_handle.write_order(), vec!["comment"]);
}

#[test]
fn worker_timeout_errors_map_to_timed_out_attempts() {
    let result =
        agent_run_result_from_error(&Error::Core(CoreError::Adapter("turn_timeout".into())));
    assert!(matches!(result.status, AttemptStatus::TimedOut));
    assert_eq!(result.error.as_deref(), Some("turn_timeout"));

    let startup_timeout =
        agent_run_result_from_error(&Error::Core(CoreError::Adapter("response_timeout".into())));
    assert!(matches!(startup_timeout.status, AttemptStatus::TimedOut));
    assert_eq!(startup_timeout.error.as_deref(), Some("response_timeout"));
}

#[tokio::test]
async fn fail_running_preserves_stalled_status() {
    let workspace_root = unique_workspace_root("finish-stalled");
    let provisioner = RecordingProvisioner::default();
    let mut service = test_service(TestTracker::new(Vec::new()), provisioner, &workspace_root);
    let issue = sample_issue("issue-4c", "FAC-4C", "Todo", "Stalled");
    let workspace_path = workspace_root.join("FAC-4C");
    service.state.running.insert(
        issue.id.clone(),
        make_running_task(issue.clone(), workspace_path),
    );
    service.claim_issue(issue.id.clone(), IssueClaimState::Running);

    service
        .fail_running(&issue.id, AttemptStatus::Stalled, "stall_timeout")
        .await;

    assert!(!service.state.running.contains_key(&issue.id));
    let retry = service.state.retrying.get(&issue.id).unwrap();
    assert_eq!(retry.row.error.as_deref(), Some("stall_timeout"));
    let context = service.state.saved_contexts.get(&issue.id).unwrap();
    assert_eq!(context.status, Some(AttemptStatus::Stalled));
    assert_eq!(context.error.as_deref(), Some("stall_timeout"));
}

#[tokio::test]
async fn run_worker_attempt_reuses_live_session_and_continues_while_issue_active() {
    let workspace_root = unique_workspace_root("worker-turns");
    let provisioner = Arc::new(RecordingProvisioner::default());
    let workspace_manager = WorkspaceManager::new(
        workspace_root.clone(),
        provisioner,
        polyphony_core::CheckoutKind::Directory,
        true,
        Vec::new(),
        None,
        None,
        None,
    );
    let issue = sample_issue("issue-turns", "FAC-TURNS", "Todo", "Loop");
    let tracker = Arc::new(SequencedStateTracker::new(issue.clone(), vec![
        "Todo",
        "Human Review",
    ]));
    let agent = Arc::new(RecordingSessionAgent::default());
    let hooks = HooksConfig {
        after_create: None,
        before_run: None,
        after_run: None,
        after_outcome: None,
        before_remove: None,
        timeout_ms: 1_000,
    };
    let (command_tx, mut command_rx) = mpsc::unbounded_channel();

    let result = run_worker_attempt(
        &workspace_manager,
        &hooks,
        agent.clone(),
        tracker,
        issue,
        Some(2),
        workspace_root.join("FAC-TURNS"),
        "Initial prompt".into(),
        vec!["Todo".into(), "In Progress".into()],
        4,
        Some(
            "Continue {{ issue.identifier }} in state {{ issue.state }}.\n\
Turn {{ turn_number }} of {{ max_turns }}. Continuation={{ is_continuation }}."
                .into(),
        ),
        polyphony_core::AgentDefinition {
            name: "codex".into(),
            kind: "codex".into(),
            transport: polyphony_core::AgentTransport::AppServer,
            ..polyphony_core::AgentDefinition::default()
        },
        None,
        watch::channel(None).1,
        command_tx,
    )
    .await
    .unwrap();

    while command_rx.try_recv().is_ok() {}

    assert!(matches!(result.status, AttemptStatus::Succeeded));
    assert_eq!(result.turns_completed, 2);
    assert_eq!(result.final_issue_state.as_deref(), Some("Human Review"));
    assert_eq!(agent.session_starts(), 1);
    assert_eq!(agent.stops(), 1);
    let prompts = agent.prompts();
    assert_eq!(prompts.len(), 2);
    assert_eq!(prompts[0], "Initial prompt");
    assert_eq!(
        prompts[1],
        "Continue FAC-TURNS in state Todo.\nTurn 2 of 4. Continuation=true."
    );
}

#[tokio::test]
async fn run_worker_attempt_does_not_continue_after_a_blocked_report() {
    let workspace_root = unique_workspace_root("blocked-live-session");
    let workspace_manager = WorkspaceManager::new(
        workspace_root.clone(),
        Arc::new(RecordingProvisioner::default()),
        polyphony_core::CheckoutKind::Directory,
        true,
        Vec::new(),
        None,
        None,
        None,
    );
    let issue = sample_issue("issue-blocked-live", "FAC-LIVE", "Todo", "Blocked live");
    let agent = Arc::new(RecordingSessionAgent::with_final_issue_state(
        "BLOCKED:\nreason: contract is missing\nevidence: disposable fixture reproduced the failure\nprerequisite: FAC-42",
    ));
    let hooks = HooksConfig {
        after_create: None,
        before_run: None,
        after_run: None,
        after_outcome: None,
        before_remove: None,
        timeout_ms: 1_000,
    };
    let (command_tx, _command_rx) = mpsc::unbounded_channel();

    let result = run_worker_attempt(
        &workspace_manager,
        &hooks,
        agent.clone(),
        Arc::new(TestTracker::new(vec![issue.clone()])),
        issue,
        None,
        workspace_root.join("FAC-LIVE"),
        "Initial prompt".into(),
        vec!["Todo".into()],
        4,
        None,
        polyphony_core::AgentDefinition::default(),
        None,
        watch::channel(None).1,
        command_tx,
    )
    .await
    .unwrap();

    assert!(result.blocked_outcome().unwrap().is_some());
    assert_eq!(agent.prompts(), vec!["Initial prompt"]);
    assert_eq!(agent.stops(), 1);
}

#[tokio::test]
async fn run_worker_attempt_stops_live_session_when_eligibility_is_revoked() {
    let workspace_root = unique_workspace_root("eligibility-stop");
    let workspace_manager = WorkspaceManager::new(
        workspace_root.clone(),
        Arc::new(RecordingProvisioner::default()),
        polyphony_core::CheckoutKind::Directory,
        true,
        Vec::new(),
        None,
        None,
        None,
    );
    let issue = sample_issue("issue-stop", "FAC-STOP", "Todo", "Stop");
    let agent = Arc::new(BlockingSessionAgent::default());
    let hooks = HooksConfig {
        after_create: None,
        before_run: None,
        after_run: None,
        after_outcome: None,
        before_remove: None,
        timeout_ms: 1_000,
    };
    let (command_tx, _command_rx) = mpsc::unbounded_channel();
    let (stop_tx, stop_rx) = watch::channel(None);
    let started = agent.started.clone();
    let worker_agent = agent.clone();
    let worker = tokio::spawn(async move {
        run_worker_attempt(
            &workspace_manager,
            &hooks,
            worker_agent,
            Arc::new(TestTracker::new(vec![issue.clone()])),
            issue,
            None,
            workspace_root.join("FAC-STOP"),
            "Initial prompt".into(),
            vec!["Todo".into()],
            1,
            None,
            polyphony_core::AgentDefinition::default(),
            None,
            stop_rx,
            command_tx,
        )
        .await
    });

    timeout(Duration::from_secs(1), started.notified())
        .await
        .expect("worker should begin its live turn");
    stop_tx
        .send(Some(
            "eligibility revoked: issue state changed to Blocked".into(),
        ))
        .unwrap();

    let result = timeout(Duration::from_secs(1), worker)
        .await
        .expect("worker should stop promptly")
        .unwrap()
        .unwrap();
    assert_eq!(result.status, AttemptStatus::CancelledByReconciliation);
    assert_eq!(
        result.error.as_deref(),
        Some("eligibility revoked: issue state changed to Blocked")
    );
    assert_eq!(agent.stops(), 1, "live session stop must be invoked");
}

#[tokio::test]
async fn run_worker_attempt_stops_before_provider_startup_when_already_revoked() {
    let workspace_root = unique_workspace_root("eligibility-stop-before-startup");
    let workspace_manager = WorkspaceManager::new(
        workspace_root.clone(),
        Arc::new(RecordingProvisioner::default()),
        polyphony_core::CheckoutKind::Directory,
        true,
        Vec::new(),
        None,
        None,
        None,
    );
    let issue = sample_issue("issue-stop-before-start", "FAC-STOP-BEFORE", "Todo", "Stop");
    let agent = Arc::new(RecordingSessionAgent::default());
    let (command_tx, _command_rx) = mpsc::unbounded_channel();
    let (stop_tx, stop_rx) = watch::channel(Some("eligibility revoked before startup".into()));
    drop(stop_tx);

    let result = run_worker_attempt(
        &workspace_manager,
        &HooksConfig {
            after_create: None,
            before_run: None,
            after_run: None,
            after_outcome: None,
            before_remove: None,
            timeout_ms: 1_000,
        },
        agent.clone(),
        Arc::new(TestTracker::new(vec![issue.clone()])),
        issue,
        None,
        workspace_root.join("FAC-STOP-BEFORE"),
        "Initial prompt".into(),
        vec!["Todo".into()],
        1,
        None,
        polyphony_core::AgentDefinition::default(),
        None,
        stop_rx,
        command_tx,
    )
    .await
    .expect("an already-revoked worker should cancel cleanly");

    assert_eq!(result.status, AttemptStatus::CancelledByReconciliation);
    assert_eq!(
        agent.session_starts(),
        0,
        "provider startup must not be called"
    );
}

#[tokio::test]
async fn run_worker_attempt_reports_live_session_termination_failure() {
    let workspace_root = unique_workspace_root("eligibility-stop-termination-failure");
    let workspace_manager = WorkspaceManager::new(
        workspace_root.clone(),
        Arc::new(RecordingProvisioner::default()),
        polyphony_core::CheckoutKind::Directory,
        true,
        Vec::new(),
        None,
        None,
        None,
    );
    let issue = sample_issue("issue-stop-fail", "FAC-STOP-FAIL", "Todo", "Stop");
    let agent = Arc::new(FailingStopSessionAgent::default());
    let (command_tx, _command_rx) = mpsc::unbounded_channel();
    let (stop_tx, stop_rx) = watch::channel(None);
    let started = agent.started.clone();
    let worker_agent = agent.clone();
    let worker = tokio::spawn(async move {
        run_worker_attempt(
            &workspace_manager,
            &HooksConfig {
                after_create: None,
                before_run: None,
                after_run: None,
                after_outcome: None,
                before_remove: None,
                timeout_ms: 1_000,
            },
            worker_agent,
            Arc::new(TestTracker::new(vec![issue.clone()])),
            issue,
            None,
            workspace_root.join("FAC-STOP-FAIL"),
            "Initial prompt".into(),
            vec!["Todo".into()],
            1,
            None,
            polyphony_core::AgentDefinition::default(),
            None,
            stop_rx,
            command_tx,
        )
        .await
    });

    timeout(Duration::from_secs(1), started.notified())
        .await
        .expect("worker should begin its live turn");
    stop_tx.send(Some("eligibility revoked".into())).unwrap();
    let error = timeout(Duration::from_secs(1), worker)
        .await
        .expect("worker should report termination failure promptly")
        .unwrap()
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("simulated process termination failure"),
        "termination failure must be surfaced instead of claiming cancellation: {error}"
    );
}

#[tokio::test]
async fn run_worker_attempt_does_not_report_clean_cancellation_when_owned_cleanup_fails() {
    let workspace_root = unique_workspace_root("eligibility-stop-owned-cleanup-failure");
    let workspace_manager = WorkspaceManager::new(
        workspace_root.clone(),
        Arc::new(RecordingProvisioner::default()),
        polyphony_core::CheckoutKind::Directory,
        true,
        Vec::new(),
        None,
        None,
        None,
    );
    let issue = sample_issue(
        "issue-stop-owned-cleanup-fail",
        "FAC-STOP-OWNED-FAIL",
        "Todo",
        "Stop",
    );
    let agent = Arc::new(FailingCancellationCleanupAgent::default());
    let (command_tx, _command_rx) = mpsc::unbounded_channel();
    let (stop_tx, stop_rx) = watch::channel(None);
    let started = agent.started.clone();
    let worker_agent = agent.clone();
    let worker = tokio::spawn(async move {
        run_worker_attempt(
            &workspace_manager,
            &HooksConfig {
                after_create: None,
                before_run: None,
                after_run: None,
                after_outcome: None,
                before_remove: None,
                timeout_ms: 1_000,
            },
            worker_agent,
            Arc::new(TestTracker::new(vec![issue.clone()])),
            issue,
            None,
            workspace_root.join("FAC-STOP-OWNED-FAIL"),
            "Initial prompt".into(),
            vec!["Todo".into()],
            1,
            None,
            polyphony_core::AgentDefinition::default(),
            None,
            stop_rx,
            command_tx,
        )
        .await
    });

    timeout(Duration::from_secs(1), started.notified())
        .await
        .expect("provider run should begin before cancellation");
    stop_tx.send(Some("eligibility revoked".into())).unwrap();
    let error = timeout(Duration::from_secs(1), worker)
        .await
        .expect("cleanup failure should be reported promptly")
        .unwrap()
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("injected owned PTY cleanup failure"),
        "failed owned cleanup must not be persisted as CancelledByReconciliation: {error}"
    );
}

#[tokio::test]
async fn run_worker_attempt_cancellation_during_startup_terminates_process() {
    let workspace_root = unique_workspace_root("eligibility-stop-startup");
    let workspace_manager = WorkspaceManager::new(
        workspace_root.clone(),
        Arc::new(RecordingProvisioner::default()),
        polyphony_core::CheckoutKind::Directory,
        true,
        Vec::new(),
        None,
        None,
        None,
    );
    let issue = sample_issue(
        "issue-startup-stop",
        "FAC-STARTUP-STOP",
        "Todo",
        "Stop startup",
    );
    let agent = Arc::new(StartupBlockingProcessAgent::default());
    let (command_tx, _command_rx) = mpsc::unbounded_channel();
    let (stop_tx, stop_rx) = watch::channel(None);
    let started = agent.started.clone();
    let worker_agent = agent.clone();
    let worker = tokio::spawn(async move {
        run_worker_attempt(
            &workspace_manager,
            &HooksConfig {
                after_create: None,
                before_run: None,
                after_run: None,
                after_outcome: None,
                before_remove: None,
                timeout_ms: 1_000,
            },
            worker_agent,
            Arc::new(TestTracker::new(vec![issue.clone()])),
            issue,
            None,
            workspace_root.join("FAC-STARTUP-STOP"),
            "Initial prompt".into(),
            vec!["Todo".into()],
            1,
            None,
            polyphony_core::AgentDefinition::default(),
            None,
            stop_rx,
            command_tx,
        )
        .await
    });

    timeout(Duration::from_secs(1), started.notified())
        .await
        .expect("fake process should start before its handshake blocks");
    let pid = agent.pid.lock().unwrap().expect("fake process pid");
    stop_tx.send(Some("eligibility revoked".into())).unwrap();
    let result = timeout(Duration::from_secs(1), worker)
        .await
        .expect("startup cancellation should complete promptly")
        .unwrap()
        .unwrap();
    assert_eq!(result.status, AttemptStatus::CancelledByReconciliation);

    timeout(Duration::from_secs(1), async {
        loop {
            let alive = std::process::Command::new("kill")
                .args(["-0", &pid.to_string()])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .map(|status| status.success())
                .unwrap_or(false);
            if !alive {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("cancelling startup must terminate the fake process pid");
}

#[tokio::test]
async fn reconcile_running_requests_stop_for_ineligible_issue_when_enabled() {
    let workspace_root = unique_workspace_root("eligibility-policy");
    let running_issue = sample_issue("issue-policy", "FAC-POLICY", "Ready", "Policy");
    let tracker_issue = sample_issue("issue-policy", "FAC-POLICY", "Backlog", "Policy");
    let tracker = TestTracker::new(vec![tracker_issue.clone()]);
    let workflow = test_workflow_with_front_matter(
        &workspace_root,
        "---\ntracker:\n  kind: mock\n  active_states: [Ready]\n  terminal_states: [Done]\n  stop_when_ineligible: true\npolling:\n  interval_ms: 1000\nworkspace:\n  root: __ROOT__\norchestration:\n  dispatch_mode: manual\nagents:\n  default: mock\n  profiles:\n    mock:\n      kind: mock\n      transport: mock\n      command: mock\n---\nTest prompt\n",
    );
    let (_tx, workflow_rx) = watch::channel(workflow);
    let mut service = RuntimeService::new(
        Arc::new(tracker),
        None,
        Arc::new(NoopAgent),
        Arc::new(RecordingProvisioner::default()),
        None,
        None,
        None,
        None,
        None,
        None,
        workflow_rx.clone(),
    )
    .0;
    let run_id = "run-eligibility-policy".to_string();
    let mut run = persisted_issue_run(&running_issue, &workspace_root, RunStatus::InProgress);
    run.id = run_id.clone();
    run.pipeline_stage = Some(PipelineStage::Executing);
    service.state.runs.insert(run_id.clone(), run);
    let task = polyphony_core::PlannedTask {
        title: "Stop on eligibility revocation".into(),
        category: "coding".into(),
        description: None,
        agent: None,
        role: polyphony_core::PipelineTaskRole::Implementation,
    }
    .to_task(&run_id, 0);
    let task_id = task.id.clone();
    service.state.tasks.insert(run_id.clone(), vec![task]);
    let mut running = make_running_task(running_issue.clone(), workspace_root.join("FAC-POLICY"));
    running.run_id = Some(run_id.clone());
    running.active_task_id = Some(task_id.clone());
    service
        .state
        .running
        .insert(running_issue.id.clone(), running);
    service.schedule_retry(
        running_issue.id.clone(),
        running_issue.identifier.clone(),
        1,
        Some("fixture retry".into()),
        false,
        1_000,
    );
    let mut stop_rx = service
        .state
        .running
        .get(&running_issue.id)
        .unwrap()
        .stop_tx
        .subscribe();

    service.reconcile_running().await;

    stop_rx.changed().await.unwrap();
    assert_eq!(
        stop_rx.borrow().as_deref(),
        Some("eligibility revoked: issue state changed to Backlog")
    );
    assert!(service.state.running.contains_key(&running_issue.id));
    assert!(!service.state.retrying.contains_key(&running_issue.id));
    assert!(
        !service.should_dispatch(&workflow_rx.borrow(), &tracker_issue),
        "an otherwise-open Backlog issue must not be dispatched when only Ready is active"
    );

    // The normal worker-completion path consumes the policy stop signal.  It
    // must terminalize this pipeline without putting any work back on a
    // planner, dispatch, retry, task, or continuation queue.
    service.state.running.remove(&running_issue.id);
    service
        .handle_task_finished(
            &workflow_rx.borrow(),
            &running_issue,
            &run_id,
            &task_id,
            &workspace_root,
            &AgentRunResult::cancelled("eligibility revoked: issue state changed to Backlog"),
            Some(0),
        )
        .await
        .unwrap();
    assert_eq!(service.state.runs[&run_id].status, RunStatus::Cancelled);
    assert_eq!(
        service.state.tasks[&run_id][0].status,
        TaskStatus::Cancelled
    );
    assert!(service.state.running.is_empty());
    assert!(service.state.retrying.is_empty());
    assert!(service.pending_manual_dispatches.is_empty());
    assert!(service.pending_webhook_dispatches.is_empty());
    assert!(service.pending_task_resolutions.is_empty());
    assert!(service.pending_task_retries.is_empty());
    assert!(service.pending_run_retries.is_empty());
    assert!(service.pending_feedback_injections.is_empty());
    assert!(
        !service.state.recent_events.iter().any(|event| {
            event.message.contains("pipeline dispatched")
                || event.message.contains("re-running planner")
        }),
        "eligibility policy cancellation must not enqueue new pipeline work"
    );
}

#[tokio::test]
async fn github_blank_project_status_poll_error_never_dispatches_or_acknowledges_retry() {
    let workspace_root = unique_workspace_root("github-blank-project-status");
    let issue = sample_issue("42", "POLY-42", "Ready", "Project status fixture");
    let tracker = GithubBlankProjectStatusTracker::with_project_status("  ");
    let workflow = test_workflow_with_front_matter(
        &workspace_root,
        "---\ntracker:\n  kind: github\n  repository: repo-owner/repo\n  api_key: test-token\n  active_states: [Ready]\n  stop_when_ineligible: true\nworkspace:\n  root: __ROOT__\nagents:\n  default: mock\n  profiles:\n    mock:\n      kind: mock\n      transport: mock\n      command: mock\n---\nTest prompt\n",
    );
    let (_tx, workflow_rx) = watch::channel(workflow);
    let mut service = RuntimeService::new(
        Arc::new(tracker.clone()),
        None,
        Arc::new(NoopAgent),
        Arc::new(RecordingProvisioner::default()),
        None,
        None,
        None,
        None,
        None,
        None,
        workflow_rx,
    )
    .0;
    service.schedule_retry(
        issue.id.clone(),
        issue.identifier.clone(),
        1,
        Some("fixture retry".into()),
        false,
        1_000,
    );

    service.handle_retry(issue.id.clone()).await;

    assert_eq!(
        tracker.candidate_polls(),
        1,
        "retry must poll the GitHub tracker"
    );
    assert!(
        service.state.running.is_empty(),
        "poll failure must not start a task"
    );
    assert!(
        service.state.runs.is_empty(),
        "poll failure must not create a run"
    );
    assert!(
        service.state.tasks.is_empty(),
        "poll failure must not create tasks"
    );
    assert!(
        service
            .state
            .recent_events
            .iter()
            .all(|event| event.scope != EventScope::Dispatch),
        "poll failure must not emit a dispatch event"
    );
    assert!(
        tracker.acknowledgements().is_empty(),
        "poll failure must not acknowledge the GitHub issue"
    );
    let retry = service
        .state
        .retrying
        .get(&issue.id)
        .expect("poll failure should safely reschedule the retry");
    assert!(
        retry
            .row
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("missing or empty"),
        "the retry must retain the GitHub Project Status diagnostic"
    );
}

#[tokio::test]
async fn saved_context_updates_from_streamed_agent_events() {
    let workspace_root = unique_workspace_root("context-events");
    let provisioner = RecordingProvisioner::default();
    let mut service = test_service(TestTracker::new(Vec::new()), provisioner, &workspace_root);
    let issue = sample_issue("issue-5", "FAC-5", "Todo", "Context");
    let workspace_path = workspace_root.join("FAC-5");
    let mut running = make_running_task(issue.clone(), workspace_path);
    running.model = Some("kimi-2.5".into());
    service.state.running.insert(issue.id.clone(), running);

    service
        .handle_message(OrchestratorMessage::AgentEvent(AgentEvent {
            issue_id: issue.id.clone(),
            issue_identifier: issue.identifier.clone(),
            agent_name: "kimi".into(),
            session_id: Some("sess-1".into()),
            thread_id: Some("thread-1".into()),
            turn_id: Some("turn-3".into()),
            codex_app_server_pid: Some("4242".into()),
            kind: AgentEventKind::Notification,
            at: Utc::now(),
            message: Some("Investigating failing test".into()),
            usage: Some(TokenUsage {
                input_tokens: 12,
                output_tokens: 8,
                total_tokens: 20,
            }),
            rate_limits: None,
            raw: None,
        }))
        .await
        .unwrap();

    let context = service.state.saved_contexts.get(&issue.id).unwrap();
    assert_eq!(context.agent_name, "kimi");
    assert_eq!(context.model.as_deref(), Some("kimi-2.5"));
    assert_eq!(context.session_id.as_deref(), Some("sess-1"));
    assert_eq!(context.thread_id.as_deref(), Some("thread-1"));
    assert_eq!(context.turn_id.as_deref(), Some("turn-3"));
    assert_eq!(context.codex_app_server_pid.as_deref(), Some("4242"));
    assert_eq!(context.usage.total_tokens, 20);
    assert_eq!(context.transcript.len(), 1);
    assert!(
        context.transcript[0]
            .message
            .contains("Investigating failing test")
    );
    let snapshot = service.snapshot();
    let running = &snapshot.running[0];
    assert_eq!(running.session_id.as_deref(), Some("sess-1"));
    assert_eq!(running.thread_id.as_deref(), Some("thread-1"));
    assert_eq!(running.turn_id.as_deref(), Some("turn-3"));
    assert_eq!(running.codex_app_server_pid.as_deref(), Some("4242"));
    let artifact_dir = workspace_root
        .join("FAC-5")
        .join(".polyphony")
        .join("runtime");
    let saved_context = tokio::fs::read_to_string(artifact_dir.join("saved-context.json"))
        .await
        .unwrap();
    let events = tokio::fs::read_to_string(artifact_dir.join("agent-events.jsonl"))
        .await
        .unwrap();
    assert!(saved_context.contains("\"issue_identifier\": \"FAC-5\""));
    assert!(events.contains("\"issue_identifier\":\"FAC-5\""));
}

#[tokio::test]
async fn retry_dispatch_rotates_to_fallback_agent_using_saved_context() {
    let workspace_root = unique_workspace_root("fallback");
    let workflow = test_workflow_with_front_matter(
        &workspace_root,
        "---\ntracker:\n  kind: mock\npolling:\n  interval_ms: 1000\nworkspace:\n  root: __ROOT__\nagents:\n  default: codex\n  profiles:\n    codex:\n      kind: codex\n      transport: app_server\n      command: codex app-server\n      fallbacks:\n        - kimi\n        - claude\n    kimi:\n      kind: kimi\n      api_key: test-kimi\n      model: kimi-2.5\n    claude:\n      kind: claude\n      transport: local_cli\n      command: claude\n---\nTest prompt\n",
    );
    let (_tx, rx) = watch::channel(workflow.clone());
    let tracker = TestTracker::new(vec![sample_issue("issue-6", "FAC-6", "Todo", "Retry")]);
    let provisioner = RecordingProvisioner::default();
    let mut service = RuntimeService::new(
        Arc::new(tracker),
        None,
        Arc::new(NoopAgent),
        Arc::new(provisioner),
        None,
        None,
        None,
        None,
        None,
        None,
        rx,
    )
    .0;
    let issue = sample_issue("issue-6", "FAC-6", "Todo", "Retry");
    service
        .state
        .saved_contexts
        .insert(issue.id.clone(), AgentContextSnapshot {
            repo_id: String::new(),
            issue_id: issue.id.clone(),
            issue_identifier: issue.identifier.clone(),
            updated_at: Utc::now(),
            agent_name: "codex".into(),
            model: Some("gpt-5-codex".into()),
            session_id: Some("session-1".into()),
            thread_id: None,
            turn_id: None,
            codex_app_server_pid: None,
            status: Some(AttemptStatus::Failed),
            error: Some("rate limited".into()),
            usage: TokenUsage::default(),
            transcript: vec![AgentContextEntry {
                at: Utc::now(),
                kind: AgentEventKind::Notification,
                message: "Partial work already completed".into(),
            }],
        });

    service
        .dispatch_issue(workflow, issue.clone(), Some(2), true, None, false, None)
        .await
        .unwrap();

    let running = service.state.running.get(&issue.id).unwrap();
    assert_eq!(running.agent_name, "kimi");
    running.handle.abort();
}

#[test]
fn rate_limited_errors_are_detected_for_fast_retry() {
    assert!(is_rate_limited_error(Some(
        "rate_limited: Claude usage limit reached"
    )));
    assert!(is_rate_limited_error(Some("quota exhausted")));
    assert!(!is_rate_limited_error(Some("response_error")));
    assert!(!is_rate_limited_error(None));
}

#[test]
fn rate_limited_retries_skip_workspace_sync() {
    assert!(should_skip_workspace_sync_for_retry(Some(
        "rate_limited: You've hit your limit"
    )));
    assert!(should_skip_workspace_sync_for_retry(Some(
        "quota exhausted"
    )));
    assert!(!should_skip_workspace_sync_for_retry(Some(
        "response_error"
    )));
    assert!(!should_skip_workspace_sync_for_retry(None));
}

#[tokio::test]
async fn tick_defensively_reloads_workflow_and_rebuilds_components() {
    let workspace_root = unique_workspace_root("workflow-reload");
    let workflow = test_workflow_with_front_matter(
        &workspace_root,
        "---\ntracker:\n  kind: mock\npolling:\n  interval_ms: 1000\nworkspace:\n  root: __ROOT__\nagents:\n  default: mock\n  profiles:\n    mock:\n      kind: mock\n      transport: mock\n      command: mock\n---\nInitial prompt\n",
    );
    let component_factory: Arc<RuntimeComponentFactory> = Arc::new(|workflow| {
        Ok(RuntimeComponents {
            tracker: Arc::new(NamedTracker::new(
                format!("tracker:{}", workflow.config.tracker.kind),
                Vec::new(),
            )),
            pull_request_event_source: None,
            agent: Arc::new(NamedAgent::new(format!(
                "agent:{}",
                workflow.config.tracker.kind
            ))),
            committer: None,
            pull_request_manager: None,
            pull_request_commenter: None,
            feedback: None,
        })
    });
    let mut service = test_service_with_reload(
        workflow.clone(),
        Arc::new(NamedTracker::new("tracker:mock", Vec::new())),
        Arc::new(NamedAgent::new("agent:mock")),
        RecordingProvisioner::default(),
        component_factory,
    );

    fs::write(
            &workflow.path,
            "---\ntracker:\n  kind: none\npolling:\n  interval_ms: 250\nworkspace:\n  root: __ROOT__\nagents:\n  default: mock\n  profiles:\n    mock:\n      kind: mock\n      transport: mock\n      command: mock\n---\nReloaded prompt\n"
                .replace("__ROOT__", &workspace_root.display().to_string()),
        )
        .unwrap();

    service.tick().await;

    assert_eq!(service.tracker.component_key(), "tracker:none");
    assert_eq!(service.agent.component_key(), "agent:none");
    assert_eq!(service.workflow().config.polling.interval_ms, 250);
    assert_eq!(
        service.workflow().definition.prompt_template,
        "Reloaded prompt"
    );
}

#[tokio::test]
async fn invalid_reloaded_workflow_blocks_dispatch_until_fixed() {
    let workspace_root = unique_workspace_root("workflow-invalid");
    let workflow = test_workflow_with_front_matter(
        &workspace_root,
        "---\ntracker:\n  kind: none\npolling:\n  interval_ms: 1000\nworkspace:\n  root: __ROOT__\nagents:\n  default: mock\n  profiles:\n    mock:\n      kind: mock\n      transport: mock\n      command: mock\n---\nPrompt\n",
    );
    let issue = sample_issue("issue-reload", "FAC-RELOAD", "Todo", "Blocked");
    let issue_for_factory = issue.clone();
    let component_factory: Arc<RuntimeComponentFactory> = Arc::new(move |workflow| {
        Ok(RuntimeComponents {
            tracker: Arc::new(NamedTracker::new(
                format!("tracker:{}", workflow.config.tracker.kind),
                vec![issue_for_factory.clone()],
            )),
            pull_request_event_source: None,
            agent: Arc::new(NamedAgent::new(format!(
                "agent:{}",
                workflow.config.tracker.kind
            ))),
            committer: None,
            pull_request_manager: None,
            pull_request_commenter: None,
            feedback: None,
        })
    });
    let mut service = test_service_with_reload(
        workflow.clone(),
        Arc::new(NamedTracker::new("tracker:none", vec![issue.clone()])),
        Arc::new(NamedAgent::new("agent:none")),
        RecordingProvisioner::default(),
        component_factory,
    );

    fs::write(&workflow.path, "---\ntracker:\n  kind: [\n").unwrap();

    service.tick().await;

    assert!(service.workflow_reload_error().is_some());
    assert!(service.state.running.is_empty());
    assert_eq!(service.workflow().definition.prompt_template, "Prompt");
}

#[test]
fn append_saved_context_includes_recent_transcript() {
    let prompt = append_saved_context(
        "Base prompt".into(),
        Some(&AgentContextSnapshot {
            repo_id: String::new(),
            issue_id: "issue-7".into(),
            issue_identifier: "FAC-7".into(),
            updated_at: Utc::now(),
            agent_name: "claude".into(),
            model: Some("claude-sonnet".into()),
            session_id: Some("session-2".into()),
            thread_id: None,
            turn_id: None,
            codex_app_server_pid: None,
            status: Some(AttemptStatus::Failed),
            error: Some("tool timeout".into()),
            usage: TokenUsage::default(),
            transcript: vec![AgentContextEntry {
                at: Utc::now(),
                kind: AgentEventKind::Notification,
                message: "Implemented parser, tests still failing".into(),
            }],
        }),
        true,
    );

    assert!(prompt.contains("## Saved Polyphony Context"));
    assert!(prompt.contains("Last agent: claude (claude-sonnet)"));
    assert!(prompt.contains("Last error: tool timeout"));
    assert!(prompt.contains("Implemented parser, tests still failing"));
}

#[tokio::test]
async fn pipeline_issue_event_creates_pull_request_deliverable_without_github() {
    let workspace_root = unique_workspace_root("pipeline-issue-pr");
    let workflow = pipeline_workflow_with_automation(&workspace_root);
    let (_tx, rx) = watch::channel(workflow.clone());
    let mut issue = sample_issue("issue-pipeline-pr", "DOG-101", "Todo", "Create e2e file");
    issue.url = Some("https://example.test/issues/DOG-101".into());
    let tracker = TestTracker::new(vec![issue.clone()]);
    let tracker_handle = tracker.clone();
    let agent = ScriptedPipelineAgent::default();
    let agent_handle = agent.clone();
    let committer = RecordingCommitter::new(Some(WorkspaceCommitResult {
        branch_name: "task/dog-101".into(),
        head_sha: "abc123def".into(),
        changed_files: 1,
        lines_added: None,
        lines_removed: None,
    }));
    let committer_handle = committer.clone();
    let pull_request_manager = RecordingPullRequestManager::new(PullRequestRef {
        repository: "penso/polyphony".into(),
        number: 17,
        url: Some("https://github.com/penso/polyphony/pull/17".into()),
    });
    let pull_request_manager_handle = pull_request_manager.clone();
    let mut service = RuntimeService::new(
        Arc::new(tracker),
        None,
        Arc::new(agent),
        Arc::new(RecordingProvisioner::default()),
        Some(Arc::new(committer)),
        Some(Arc::new(pull_request_manager)),
        None,
        None,
        None,
        None,
        rx,
    )
    .0;

    service
        .dispatch_issue(
            workflow.clone(),
            issue.clone(),
            None,
            false,
            None,
            false,
            None,
        )
        .await
        .unwrap();
    handle_next_worker_message(&mut service).await;
    handle_next_worker_message(&mut service).await;

    let run = service
        .state
        .runs
        .values()
        .find(|run| run.issue_id.as_deref() == Some(issue.id.as_str()))
        .cloned()
        .expect("issue run missing after pipeline completion");
    assert_eq!(run.status, RunStatus::Delivered);
    let deliverable = run
        .deliverable
        .expect("run should record the pull request deliverable");
    assert_eq!(deliverable.kind, DeliverableKind::GithubPullRequest);
    assert_eq!(deliverable.status, DeliverableStatus::Open);
    assert_eq!(
        deliverable.url.as_deref(),
        Some("https://github.com/penso/polyphony/pull/17")
    );
    assert_eq!(tracker_handle.recorded_workflow_updates(), vec![
        "In Progress",
        "Done"
    ]);
    assert_eq!(committer_handle.requests().len(), 1);
    assert_eq!(
        committer_handle.requests()[0].base_branch.as_deref(),
        Some("main")
    );
    assert_eq!(pull_request_manager_handle.requests().len(), 1);
    assert_eq!(
        pull_request_manager_handle.requests()[0].head_branch,
        "task/dog-101"
    );
    assert_eq!(
        pull_request_manager_handle.requests()[0].title,
        "DOG-101: Create e2e file"
    );
    assert_eq!(agent_handle.recorded_agent_names(), vec![
        "router",
        "implementer",
        "implementer"
    ]);
    assert_eq!(
        tokio::fs::read_to_string(workspace_root.join("DOG-101").join("e2e-pr.txt"))
            .await
            .unwrap(),
        "polyphony end-to-end dogfood\n"
    );

    let snapshot = service.snapshot();
    let run_row = snapshot
        .runs
        .iter()
        .find(|run| run.issue_identifier.as_deref() == Some("DOG-101"))
        .expect("run row missing from runtime snapshot");
    assert_eq!(run_row.status, RunStatus::Delivered);
    assert!(run_row.has_deliverable);
}

#[tokio::test]
async fn cancelled_pipeline_planner_is_terminal_and_never_creates_tasks() {
    let workspace_root = unique_workspace_root("cancelled-pipeline-planner");
    let workflow = pipeline_workflow_with_automation(&workspace_root);
    let issue = sample_issue(
        "issue-cancelled-planner",
        "DOG-808",
        "Todo",
        "Cancel planner",
    );
    let mut service = test_service(
        TestTracker::new(vec![issue.clone()]),
        RecordingProvisioner::default(),
        &workspace_root,
    );
    let now = Utc::now();
    service
        .state
        .runs
        .insert("run-cancelled-planner".into(), Run {
            id: "run-cancelled-planner".into(),
            kind: RunKind::IssueDelivery,
            issue_id: Some(issue.id.clone()),
            issue_identifier: Some(issue.identifier.clone()),
            title: issue.title.clone(),
            status: RunStatus::Planning,
            pipeline_stage: Some(PipelineStage::Planning),
            manual_dispatch_directives: None,
            workspace_key: None,
            workspace_path: Some(workspace_root.clone()),
            review_target: None,
            deliverable: None,
            created_at: now,
            activity_log: Vec::new(),
            cancel_reason: None,
            blocked_outcome: None,
            steps: polyphony_core::build_planner_steps(),
            updated_at: now,
        });

    service
        .handle_planner_finished(
            &workflow,
            &issue,
            "run-cancelled-planner",
            &workspace_root,
            &AgentRunResult::cancelled("eligibility revoked"),
            None,
        )
        .await
        .expect("cancellation should be recorded without dispatching");

    let run = service.state.runs.get("run-cancelled-planner").unwrap();
    assert_eq!(run.status, RunStatus::Cancelled);
    assert_eq!(run.cancel_reason.as_deref(), Some("eligibility revoked"));
    assert!(
        run.steps
            .iter()
            .all(|step| step.status == StepStatus::Skipped)
    );
    assert!(
        !service.state.tasks.contains_key("run-cancelled-planner"),
        "a cancelled planner must not create or dispatch pipeline tasks"
    );
}

#[tokio::test]
async fn cancelled_pipeline_task_is_terminal_for_reconciliation_and_user_stops() {
    let workspace_root = unique_workspace_root("cancelled-pipeline-task");
    let workflow = pipeline_workflow_with_automation(&workspace_root);
    let issue = sample_issue("issue-cancelled-task", "DOG-809", "Todo", "Cancel task");

    // A reconciliation cancellation arrives through the normal pipeline task
    // completion path. It must not re-plan, dispatch another task, or queue a
    // retry, and the UI snapshot must retain the cancellation reason.
    let mut service = test_service(
        TestTracker::new(vec![issue.clone()]),
        RecordingProvisioner::default(),
        &workspace_root,
    );
    let run_id = "run-cancelled-task-reconcile".to_string();
    let mut run = persisted_issue_run(&issue, &workspace_root, RunStatus::InProgress);
    run.id = run_id.clone();
    run.pipeline_stage = Some(PipelineStage::Executing);
    service.state.runs.insert(run_id.clone(), run);
    let task = polyphony_core::PlannedTask {
        title: "Do not resume".into(),
        category: "coding".into(),
        description: None,
        agent: None,
        role: polyphony_core::PipelineTaskRole::Implementation,
    }
    .to_task(&run_id, 0);
    let task_id = task.id.clone();
    service.state.tasks.insert(run_id.clone(), vec![task]);
    service
        .handle_task_finished(
            &workflow,
            &issue,
            &run_id,
            &task_id,
            &workspace_root,
            &AgentRunResult::cancelled("eligibility revoked: issue moved to Backlog"),
            Some(0),
        )
        .await
        .unwrap();
    assert_eq!(service.state.runs[&run_id].status, RunStatus::Cancelled);
    assert_eq!(
        service.state.tasks[&run_id][0].status,
        TaskStatus::Cancelled
    );
    assert!(service.state.running.is_empty());
    assert!(service.state.retrying.is_empty());
    assert!(service.pending_manual_dispatches.is_empty());
    assert!(service.pending_webhook_dispatches.is_empty());
    assert!(service.pending_task_retries.is_empty());
    assert!(service.pending_run_retries.is_empty());
    assert!(service.pending_feedback_injections.is_empty());
    assert!(
        !service
            .state
            .recent_events
            .iter()
            .any(|event| event.message.contains("pipeline dispatched")
                || event.message.contains("re-running planner")),
        "a reconciliation cancellation must not queue dispatch or planner work"
    );
    let snapshot = service.snapshot();
    assert_eq!(
        snapshot
            .runs
            .iter()
            .find(|row| row.id == run_id)
            .unwrap()
            .cancel_reason
            .as_deref(),
        Some("eligibility revoked: issue moved to Backlog")
    );
    assert_eq!(
        snapshot
            .tasks
            .iter()
            .find(|row| row.id == task_id)
            .unwrap()
            .status,
        TaskStatus::Cancelled
    );

    // User cancellation removes the worker immediately, so its task must be
    // made terminal by stop_running_by_user rather than waiting for a worker
    // completion callback that will never arrive.
    let mut service = test_service(
        TestTracker::new(vec![issue.clone()]),
        RecordingProvisioner::default(),
        &workspace_root,
    );
    let user_run_id = "run-cancelled-task-user".to_string();
    let mut user_run = persisted_issue_run(&issue, &workspace_root, RunStatus::InProgress);
    user_run.id = user_run_id.clone();
    user_run.pipeline_stage = Some(PipelineStage::Executing);
    service.state.runs.insert(user_run_id.clone(), user_run);
    let user_task = polyphony_core::PlannedTask {
        title: "Stopped by user".into(),
        category: "coding".into(),
        description: None,
        agent: None,
        role: polyphony_core::PipelineTaskRole::Implementation,
    }
    .to_task(&user_run_id, 0);
    let user_task_id = user_task.id.clone();
    service
        .state
        .tasks
        .insert(user_run_id.clone(), vec![user_task]);
    let mut running = make_running_task(issue.clone(), workspace_root.join("DOG-809"));
    running.run_id = Some(user_run_id.clone());
    running.active_task_id = Some(user_task_id.clone());
    service.state.running.insert(issue.id.clone(), running);
    service.stop_running_by_user(&issue.id).await;

    // Exercise the next polling tick after the user stop. The cancelled task
    // must stay terminal rather than being resolved, continued, retried, or
    // sent back through planner/worker dispatch.
    service.tick().await;

    assert_eq!(
        service.state.runs[&user_run_id].status,
        RunStatus::Cancelled
    );
    assert_eq!(
        service.state.tasks[&user_run_id][0].status,
        TaskStatus::Cancelled
    );
    assert!(service.state.running.is_empty());
    assert!(service.state.retrying.is_empty());
    assert!(service.pending_manual_dispatches.is_empty());
    assert!(service.pending_webhook_dispatches.is_empty());
    assert!(
        service
            .pending_manual_pull_request_inbox_dispatches
            .is_empty()
    );
    assert!(service.pending_task_resolutions.is_empty());
    assert!(service.pending_task_retries.is_empty());
    assert!(service.pending_run_retries.is_empty());
    assert!(service.pending_feedback_injections.is_empty());
    assert!(service.pending_agent_stops.is_empty());
    assert!(
        service
            .state
            .recent_events
            .iter()
            .any(|event| { event.message == format!("user stopped {}", issue.identifier) }),
        "the user-stop audit event should remain visible after the polling tick"
    );
    assert!(
        !service.state.recent_events.iter().any(|event| {
            event.message.contains("pipeline dispatched")
                || event.message.contains("re-running planner")
                || event.message.contains("dispatch failed")
        }),
        "a user-stopped pipeline task must not produce worker, planner, or replan dispatch events"
    );
    let snapshot = service.snapshot();
    let snapshot_run = snapshot
        .runs
        .iter()
        .find(|row| row.id == user_run_id)
        .unwrap();
    assert_eq!(snapshot_run.status, RunStatus::Cancelled);
    assert_eq!(
        snapshot_run.cancel_reason.as_deref(),
        Some("stopped by user")
    );
    let snapshot_task = snapshot
        .tasks
        .iter()
        .find(|row| row.id == user_task_id)
        .unwrap();
    assert_eq!(snapshot_task.status, TaskStatus::Cancelled);
    assert_eq!(snapshot_task.error.as_deref(), Some("stopped by user"));
    let history = snapshot.agent_run_history.first().unwrap();
    assert_eq!(history.status, AttemptStatus::CancelledByUser);
    assert_eq!(history.error.as_deref(), Some("stopped by user"));
}

#[tokio::test]
async fn failed_pipeline_task_replans_when_configured() {
    let workspace_root = unique_workspace_root("pipeline-replan-after-failure");
    let mut workflow = pipeline_workflow_with_automation(&workspace_root);
    workflow.config.pipeline.replan_on_failure = true;
    let issue = sample_issue("issue-replan", "DOG-810", "Todo", "Replan after failure");
    let mut service = test_service_for_workflow(
        workflow.clone(),
        TestTracker::new(vec![issue.clone()]),
        RecordingProvisioner::default(),
    );
    let run_id = "run-replan-after-failure".to_string();
    let mut run = persisted_issue_run(&issue, &workspace_root, RunStatus::InProgress);
    run.id = run_id.clone();
    run.pipeline_stage = Some(PipelineStage::Executing);
    service.state.runs.insert(run_id.clone(), run);
    let task = polyphony_core::PlannedTask {
        title: "Fail then replan".into(),
        category: "coding".into(),
        description: None,
        agent: None,
        role: polyphony_core::PipelineTaskRole::Implementation,
    }
    .to_task(&run_id, 0);
    let task_id = task.id.clone();
    service.state.tasks.insert(run_id.clone(), vec![task]);

    service
        .handle_task_finished(
            &workflow,
            &issue,
            &run_id,
            &task_id,
            &workspace_root,
            &AgentRunResult::failed("ordinary worker failure"),
            Some(0),
        )
        .await
        .unwrap();

    assert_eq!(service.state.runs[&run_id].status, RunStatus::Planning);
    assert!(service.state.running.contains_key(&issue.id));
    assert_eq!(
        service.state.running[&issue.id].agent_name, "router",
        "ordinary task failure must redispatch the planner when replan_on_failure is enabled"
    );
    assert!(service.state.recent_events.iter().any(|event| {
        event.message.contains("task failed, re-running planner")
            && event.message.contains(&issue.identifier)
    }));
}

#[tokio::test]
async fn pipeline_issue_event_writes_workspace_artifacts_and_runs_after_outcome_hook() {
    let workspace_root = unique_workspace_root("pipeline-issue-artifacts");
    let mut workflow = pipeline_workflow_with_automation(&workspace_root);
    workflow.config.hooks.after_outcome = Some("printf cleaned > .after_outcome".into());
    let (_tx, rx) = watch::channel(workflow.clone());
    let mut issue = sample_issue(
        "issue-pipeline-artifacts",
        "DOG-103",
        "Todo",
        "Archive artifacts",
    );
    issue.url = Some("https://example.test/issues/DOG-103".into());
    let tracker = TestTracker::new(vec![issue.clone()]);
    let agent = ScriptedPipelineAgent::default();
    let committer = RecordingCommitter::new(Some(WorkspaceCommitResult {
        branch_name: "task/dog-103".into(),
        head_sha: "abc123def".into(),
        changed_files: 1,
        lines_added: None,
        lines_removed: None,
    }));
    let pull_request_manager = RecordingPullRequestManager::new(PullRequestRef {
        repository: "penso/polyphony".into(),
        number: 23,
        url: Some("https://github.com/penso/polyphony/pull/23".into()),
    });
    let mut service = RuntimeService::new(
        Arc::new(tracker),
        None,
        Arc::new(agent),
        Arc::new(RecordingProvisioner::default()),
        Some(Arc::new(committer)),
        Some(Arc::new(pull_request_manager)),
        None,
        None,
        None,
        None,
        rx,
    )
    .0;

    service
        .dispatch_issue(workflow, issue, None, false, None, false, None)
        .await
        .unwrap();
    handle_next_worker_message(&mut service).await;
    handle_next_worker_message(&mut service).await;

    let workspace_path = workspace_root.join("DOG-103");
    let artifact_dir = workspace_path.join(".polyphony").join("runtime");
    assert_eq!(
        tokio::fs::read_to_string(workspace_path.join(".after_outcome"))
            .await
            .unwrap(),
        "cleaned"
    );
    let saved_context = tokio::fs::read_to_string(artifact_dir.join("saved-context.json"))
        .await
        .unwrap();
    let runs = tokio::fs::read_to_string(artifact_dir.join("agent-run-history.jsonl"))
        .await
        .unwrap();
    assert!(saved_context.contains("\"issue_identifier\": \"DOG-103\""));
    assert!(runs.contains("\"issue_identifier\":\"DOG-103\""));
}

#[test]
fn restore_bootstrap_rehydrates_saved_context_from_workspace_artifact() {
    let workspace_root = unique_workspace_root("restore-context-artifact");
    let tracker = TestTracker::new(Vec::new());
    let provisioner = RecordingProvisioner::default();
    let mut service = test_service(tracker, provisioner, &workspace_root);
    let workspace_path = workspace_root.join("DOG-104");
    std::fs::create_dir_all(workspace_path.join(".polyphony/runtime")).unwrap();
    let now = Utc::now();
    let context = AgentContextSnapshot {
        repo_id: String::new(),
        issue_id: "issue-restore".into(),
        issue_identifier: "DOG-104".into(),
        updated_at: now,
        agent_name: "implementer".into(),
        model: None,
        session_id: None,
        thread_id: None,
        turn_id: None,
        codex_app_server_pid: None,
        status: Some(AttemptStatus::Succeeded),
        error: None,
        usage: TokenUsage::default(),
        transcript: vec![AgentContextEntry {
            at: now,
            kind: AgentEventKind::Notification,
            message: "rehydrated from workspace".into(),
        }],
    };
    std::fs::write(
        polyphony_core::workspace_saved_context_artifact_path(&workspace_path),
        serde_json::to_vec_pretty(&context).unwrap(),
    )
    .unwrap();

    service.restore_bootstrap(StoreBootstrap {
        snapshot: Some(RuntimeSnapshot {
            repo_ids: Vec::new(),
            repo_registrations: Vec::new(),
            generated_at: now,
            counts: SnapshotCounts::default(),
            cadence: RuntimeCadence::default(),
            tracker_issues: Vec::new(),
            inbox_items: Vec::new(),
            approved_inbox_keys: Vec::new(),
            running: Vec::new(),
            agent_run_history: Vec::new(),
            retrying: Vec::new(),
            codex_totals: CodexTotals::default(),
            rate_limits: None,
            throttles: Vec::new(),
            budgets: Vec::new(),
            agent_catalogs: Vec::new(),
            saved_contexts: vec![saved_context_metadata(AgentContextSnapshot {
                transcript: Vec::new(),
                ..context.clone()
            })],
            recent_events: Vec::new(),
            pending_user_interactions: Vec::new(),
            runs: Vec::new(),
            tasks: Vec::new(),
            loading: LoadingState::default(),
            dispatch_mode: DispatchMode::default(),
            tracker_kind: TrackerKind::default(),
            tracker_connection: None,
            from_cache: false,
            cached_at: None,
            agent_profile_names: Vec::new(),
            agent_profiles: Vec::new(),
            heartbeat: polyphony_core::HeartbeatStatus::default(),
        }),
        retrying: std::collections::HashMap::new(),
        throttles: std::collections::HashMap::new(),
        budgets: std::collections::HashMap::new(),
        saved_contexts: std::collections::HashMap::new(),
        recent_events: Vec::new(),
        runs: std::collections::HashMap::new(),
        tasks: std::collections::HashMap::new(),
        reviewed_pull_request_heads: std::collections::HashMap::new(),
        agent_run_history: vec![PersistedAgentRunRecord {
            repo_id: String::new(),
            run_id: Some("run-restore".into()),
            issue_id: "issue-restore".into(),
            issue_identifier: "DOG-104".into(),
            agent_name: "implementer".into(),
            model: None,
            session_id: None,
            thread_id: None,
            turn_id: None,
            codex_app_server_pid: None,
            status: AttemptStatus::Succeeded,
            attempt: Some(1),
            max_turns: 3,
            turn_count: 1,
            last_event: None,
            last_message: None,
            started_at: now,
            finished_at: Some(now),
            last_event_at: Some(now),
            tokens: TokenUsage::default(),
            workspace_path: Some(workspace_path.clone()),
            error: None,
            saved_context: None,
        }],
    });

    let restored = service.state.saved_contexts.get("issue-restore").unwrap();
    assert_eq!(restored.transcript.len(), 1);
    assert_eq!(restored.transcript[0].message, "rehydrated from workspace");
}

#[tokio::test]
async fn pipeline_issue_event_can_finish_without_opening_a_pull_request() {
    let workspace_root = unique_workspace_root("pipeline-issue-no-pr");
    let workflow = pipeline_workflow_with_automation(&workspace_root);
    let (_tx, rx) = watch::channel(workflow.clone());
    let issue = sample_issue(
        "issue-pipeline-clean",
        "DOG-102",
        "Todo",
        "Workspace already done",
    );
    let tracker = TestTracker::new(vec![issue.clone()]);
    let tracker_handle = tracker.clone();
    let agent = ScriptedPipelineAgent::default();
    let agent_handle = agent.clone();
    let committer = RecordingCommitter::new(None);
    let committer_handle = committer.clone();
    let pull_request_manager = RecordingPullRequestManager::new(PullRequestRef {
        repository: "penso/polyphony".into(),
        number: 99,
        url: Some("https://github.com/penso/polyphony/pull/99".into()),
    });
    let pull_request_manager_handle = pull_request_manager.clone();
    let mut service = RuntimeService::new(
        Arc::new(tracker),
        None,
        Arc::new(agent),
        Arc::new(RecordingProvisioner::default()),
        Some(Arc::new(committer)),
        Some(Arc::new(pull_request_manager)),
        None,
        None,
        None,
        None,
        rx,
    )
    .0;

    service
        .dispatch_issue(workflow, issue.clone(), None, false, None, false, None)
        .await
        .unwrap();
    handle_next_worker_message(&mut service).await;
    handle_next_worker_message(&mut service).await;

    let run = service
        .state
        .runs
        .values()
        .find(|run| run.issue_id.as_deref() == Some(issue.id.as_str()))
        .cloned()
        .expect("issue run missing after clean pipeline completion");
    assert_eq!(run.status, RunStatus::Review);
    assert!(run.deliverable.is_none());
    assert_eq!(tracker_handle.recorded_workflow_updates(), vec![
        "In Progress",
        "Done"
    ]);
    assert_eq!(committer_handle.requests().len(), 1);
    assert!(pull_request_manager_handle.requests().is_empty());
    assert_eq!(agent_handle.recorded_agent_names(), vec![
        "router",
        "implementer"
    ]);

    let snapshot = service.snapshot();
    let run_row = snapshot
        .runs
        .iter()
        .find(|run| run.issue_identifier.as_deref() == Some("DOG-102"))
        .expect("run row missing from runtime snapshot");
    assert_eq!(run_row.status, RunStatus::Review);
    assert!(!run_row.has_deliverable);
}

#[tokio::test]
async fn resolving_run_deliverable_updates_decision_and_snapshot() {
    let workspace_root = unique_workspace_root("deliverable-decision");
    let tracker = TestTracker::new(vec![sample_issue("github:7", "#7", "Todo", "Need a PR")]);
    let provisioner = RecordingProvisioner::default();
    let mut service = test_service(tracker, provisioner, &workspace_root);
    let now = Utc::now();
    service.state.runs.insert("run-1".into(), Run {
        id: "run-1".into(),
        kind: RunKind::IssueDelivery,
        issue_id: Some("github:7".into()),
        issue_identifier: Some("#7".into()),
        title: "Need a PR".into(),
        status: RunStatus::Delivered,
        pipeline_stage: None,
        manual_dispatch_directives: None,
        workspace_key: Some("_7".into()),
        workspace_path: Some(workspace_root.join("_7")),
        review_target: None,
        deliverable: Some(Deliverable {
            kind: DeliverableKind::GithubPullRequest,
            status: DeliverableStatus::Open,
            url: Some("https://github.com/penso/polyphony/pull/8".into()),
            decision: DeliverableDecision::Waiting,
            title: None,
            description: None,
            metadata: Default::default(),
        }),
        created_at: now,
        activity_log: Vec::new(),
        cancel_reason: None,
        blocked_outcome: None,
        steps: Vec::new(),
        updated_at: now,
    });

    service
        .pending_deliverable_resolutions
        .push(("run-1".into(), DeliverableDecision::Accepted));
    service.process_pending_deliverable_resolutions().await;

    let run = service.state.runs.get("run-1").expect("run exists");
    let deliverable = run
        .deliverable
        .as_ref()
        .expect("deliverable exists after resolution");
    assert_eq!(deliverable.decision, DeliverableDecision::Accepted);

    let snapshot = service.snapshot();
    let row = snapshot.runs.first().expect("run row exists");
    assert_eq!(
        row.deliverable
            .as_ref()
            .expect("deliverable row exists")
            .decision,
        DeliverableDecision::Accepted
    );
}

#[tokio::test]
async fn resolving_already_accepted_deliverable_is_ignored() {
    let workspace_root = unique_workspace_root("deliverable-decision-ignored");
    let tracker = TestTracker::new(vec![sample_issue("github:7", "#7", "Todo", "Need a PR")]);
    let provisioner = RecordingProvisioner::default();
    let mut service = test_service(tracker, provisioner, &workspace_root);
    let now = Utc::now();
    service.state.runs.insert("run-1".into(), Run {
        id: "run-1".into(),
        kind: RunKind::IssueDelivery,
        issue_id: Some("github:7".into()),
        issue_identifier: Some("#7".into()),
        title: "Need a PR".into(),
        status: RunStatus::Delivered,
        pipeline_stage: None,
        manual_dispatch_directives: None,
        workspace_key: Some("_7".into()),
        workspace_path: Some(workspace_root.join("_7")),
        review_target: None,
        deliverable: Some(Deliverable {
            kind: DeliverableKind::Patch,
            status: DeliverableStatus::Open,
            url: None,
            decision: DeliverableDecision::Accepted,
            title: None,
            description: None,
            metadata: Default::default(),
        }),
        created_at: now,
        activity_log: Vec::new(),
        cancel_reason: None,
        blocked_outcome: None,
        steps: Vec::new(),
        updated_at: now,
    });

    service
        .pending_deliverable_resolutions
        .push(("run-1".into(), DeliverableDecision::Accepted));
    service.process_pending_deliverable_resolutions().await;

    let run = service.state.runs.get("run-1").expect("run exists");
    let deliverable = run
        .deliverable
        .as_ref()
        .expect("deliverable exists after ignored resolution");
    assert_eq!(deliverable.decision, DeliverableDecision::Accepted);
    assert_eq!(
        service
            .state
            .recent_events
            .front()
            .expect("ignored event recorded")
            .message,
        "deliverable decision ignored: #7 already accepted"
    );
}

#[tokio::test]
async fn startup_cleanup_finalizes_merged_accepted_runs() {
    let workspace_root = unique_workspace_root("startup-finalize-accepted");
    let tracker = TestTracker::new(vec![sample_issue("github:7", "#7", "Todo", "Need a PR")]);
    let tracker_for_assertions = tracker.clone();
    let provisioner = RecordingProvisioner::default();
    let provisioner_for_assertions = provisioner.clone();
    let mut service = test_service(tracker, provisioner, &workspace_root);
    let now = Utc::now();
    service.state.runs.insert("run-1".into(), Run {
        id: "run-1".into(),
        kind: RunKind::IssueDelivery,
        issue_id: Some("github:7".into()),
        issue_identifier: Some("#7".into()),
        title: "Need a PR".into(),
        status: RunStatus::Delivered,
        pipeline_stage: None,
        manual_dispatch_directives: None,
        workspace_key: Some("_7".into()),
        workspace_path: Some(workspace_root.join("_7")),
        review_target: None,
        deliverable: Some(Deliverable {
            kind: DeliverableKind::LocalBranch,
            status: DeliverableStatus::Merged,
            url: None,
            decision: DeliverableDecision::Accepted,
            title: Some("Branch: task/7".into()),
            description: None,
            metadata: std::collections::HashMap::from([(
                "branch".into(),
                serde_json::Value::String("task/7".into()),
            )]),
        }),
        created_at: now,
        activity_log: Vec::new(),
        cancel_reason: None,
        blocked_outcome: None,
        steps: Vec::new(),
        updated_at: now,
    });
    service.state.worktree_keys.insert("_7".into());
    tokio::fs::create_dir_all(workspace_root.join("_7"))
        .await
        .expect("workspace directory created");

    service.startup_cleanup().await;

    assert_eq!(
        tracker_for_assertions
            .issues
            .lock()
            .unwrap()
            .get("github:7")
            .expect("issue exists")
            .state,
        "Closed"
    );
    assert_eq!(
        tracker_for_assertions.recorded_issue_updates().len(),
        1,
        "startup cleanup should close the tracker issue once"
    );
    assert_eq!(
        provisioner_for_assertions.cleaned_issue_identifiers(),
        vec!["#7".to_string()],
    );
    assert!(
        !service.state.worktree_keys.contains("_7"),
        "startup cleanup should remove the cleaned worktree key"
    );
}

// ---------------------------------------------------------------------------
// Stop mode tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn stop_mode_skips_dispatch_on_tick() {
    let workspace_root = unique_workspace_root("stop-tick");
    let tracker = TestTracker::new(vec![sample_issue("issue-1", "FAC-1", "Todo", "First")]);
    let provisioner = RecordingProvisioner::default();
    let mut service = test_service(tracker, provisioner, &workspace_root);
    service.state.dispatch_mode = polyphony_core::DispatchMode::Stop;

    service.tick().await;

    assert!(
        !service.state.running.contains_key("issue-1"),
        "stop mode should prevent dispatch"
    );
}

#[tokio::test]
async fn manual_dispatch_works_in_stop_mode() {
    let workspace_root = unique_workspace_root("stop-manual");
    let tracker = TestTracker::new(vec![sample_issue("issue-1", "FAC-1", "Todo", "First")]);
    let provisioner = RecordingProvisioner::default();
    let mut service = test_service(tracker, provisioner, &workspace_root);
    service.state.dispatch_mode = polyphony_core::DispatchMode::Stop;
    service
        .pending_manual_dispatches
        .push(crate::ManualDispatchRequest {
            issue_id: "issue-1".into(),
            agent_name: None,
            directives: None,
        });

    service.process_manual_dispatches().await;

    assert!(
        service.state.running.contains_key("issue-1"),
        "manual dispatch should work even in stop mode"
    );
}

#[tokio::test]
async fn stop_mode_blocks_automatic_dispatch() {
    let workspace_root = unique_workspace_root("stop-auto");
    let tracker = TestTracker::new(vec![sample_issue("issue-1", "FAC-1", "Todo", "Auto issue")]);
    let mut service = test_service(tracker, RecordingProvisioner::default(), &workspace_root);
    service.state.dispatch_mode = polyphony_core::DispatchMode::Stop;

    service.tick().await;

    assert!(
        !service.state.running.contains_key("issue-1"),
        "stop mode should block automatic dispatch"
    );
}

#[tokio::test]
async fn manual_dispatch_is_processed_on_next_tick() {
    let workspace_root = unique_workspace_root("manual-tick");
    let issue = sample_issue("issue-tick-1", "FAC-TICK-1", "Todo", "Tick test");
    let tracker = TestTracker::new(vec![issue.clone()]);
    let mut service = test_service(tracker, RecordingProvisioner::default(), &workspace_root);

    // Queue a manual dispatch
    service
        .pending_manual_dispatches
        .push(crate::ManualDispatchRequest {
            issue_id: issue.id.clone(),
            agent_name: None,
            directives: None,
        });

    // A single tick should process it
    service.tick().await;

    assert!(
        service.state.running.contains_key(&issue.id),
        "manual dispatch should be processed on the immediate next tick"
    );
}

#[tokio::test]
async fn manual_pull_request_dispatch_failure_creates_visible_failed_run() {
    let workspace_root = unique_workspace_root("manual-pr-dispatch-failure");
    let workflow = test_workflow_with_front_matter(
        &workspace_root,
        "---\ntracker:\n  kind: github\n  repository: penso/polyphony\n  api_key: token\npolling:\n  interval_ms: 1000\nworkspace:\n  root: __ROOT__\nagents:\n  default: reviewer\n  profiles:\n    reviewer:\n      kind: claude\n      transport: local_cli\n      command: claude -p --verbose --dangerously-skip-permissions\nreview_events:\n  pr_reviews:\n    enabled: true\n    agent: reviewer\n    debounce_seconds: 1\n---\nPrompt\n",
    );
    let (_tx, rx) = watch::channel(workflow.clone());
    let event = PullRequestReviewEvent {
        provider: polyphony_core::ReviewProviderKind::Github,
        repository: "penso/polyphony".into(),
        number: 89,
        title: "Review me".into(),
        url: Some("https://github.com/penso/polyphony/pull/89".into()),
        base_branch: "main".into(),
        head_branch: "feature/review".into(),
        head_sha: "abc123".into(),
        checkout_ref: Some("refs/pull/89/head".into()),
        author_login: Some("alice".into()),
        approval_state: DispatchApprovalState::Approved,
        labels: vec!["ready".into()],
        created_at: Some(Utc::now() - chrono::Duration::minutes(5)),
        updated_at: Some(Utc::now() - chrono::Duration::seconds(10)),
        is_draft: false,
    };
    let issue = synthetic_issue_for_pull_request_review(&event);
    let mut service = RuntimeService::new(
        Arc::new(TestTracker::new(Vec::new())),
        None,
        Arc::new(NoopAgent),
        Arc::new(FailingProvisioner {
            message: "ssh auth failed".into(),
        }),
        None,
        None,
        None,
        None,
        None,
        None,
        rx,
    )
    .0;
    let dispatch = service
        .dispatch_pull_request_review(workflow, event, None, Some("Check auth"))
        .await;
    assert!(
        dispatch.is_err(),
        "workspace setup should fail in this test"
    );

    let (run_id, run) = service
        .state
        .runs
        .iter()
        .next()
        .expect("run should be created before workspace setup succeeds");
    assert_eq!(run.issue_id.as_deref(), Some(issue.id.as_str()));
    assert_eq!(run.status, RunStatus::Failed);
    assert_eq!(
        run.manual_dispatch_directives.as_deref(),
        Some("Check auth")
    );
    let tasks = service
        .state
        .tasks
        .get(run_id)
        .expect("tasks should exist for PR dispatch");
    assert_eq!(tasks.len(), 2);
    assert_eq!(tasks[0].title, "Creating worktree");
    assert_eq!(tasks[0].status, TaskStatus::Failed);
    assert_eq!(tasks[1].title, "Run PR review");
    assert_eq!(tasks[1].status, TaskStatus::Cancelled);
    assert!(
        !service.state.running.contains_key(&issue.id),
        "worker should not start when workspace setup fails"
    );
}

#[tokio::test]
async fn workspace_progress_updates_are_appended_to_worktree_task() {
    let workspace_root = unique_workspace_root("workspace-progress");
    let provisioner = RecordingProvisioner::default();
    let mut service = test_service(TestTracker::new(Vec::new()), provisioner, &workspace_root);
    let now = Utc::now();
    let run_id = "run-progress-1".to_string();
    let task_id = "task-progress-1".to_string();
    let issue_identifier = "penso/polyphony#89".to_string();
    let workspace_key = "penso_polyphony_89".to_string();

    service.state.runs.insert(run_id.clone(), Run {
        id: run_id.clone(),
        kind: RunKind::PullRequestReview,
        issue_id: Some("pr_review:github:penso/polyphony:89:head".into()),
        issue_identifier: Some(issue_identifier.clone()),
        title: "Review me".into(),
        status: RunStatus::InProgress,
        pipeline_stage: None,
        manual_dispatch_directives: None,
        workspace_key: Some(workspace_key.clone()),
        workspace_path: None,
        review_target: None,
        deliverable: None,
        created_at: now,
        activity_log: Vec::new(),
        cancel_reason: None,
        blocked_outcome: None,
        steps: Vec::new(),
        updated_at: now,
    });
    service.state.tasks.insert(run_id.clone(), vec![Task {
        id: task_id.clone(),
        run_id: run_id.clone(),
        title: "Creating worktree".into(),
        description: None,
        activity_log: Vec::new(),
        category: polyphony_core::TaskCategory::Research,
        role: polyphony_core::PipelineTaskRole::Implementation,
        status: TaskStatus::InProgress,
        ordinal: 0,
        parent_id: None,
        agent_name: Some("orchestrator".into()),
        session_id: None,
        thread_id: None,
        turns_completed: 0,
        tokens: TokenUsage::default(),
        started_at: Some(now),
        finished_at: None,
        error: None,
        created_at: now,
        updated_at: now,
    }]);
    service
        .state
        .workspace_setup_tasks_by_issue_identifier
        .insert(issue_identifier.clone(), (run_id.clone(), task_id.clone()));
    service
        .state
        .workspace_setup_tasks_by_key
        .insert(workspace_key.clone(), (run_id.clone(), task_id.clone()));

    let update = WorkspaceProgressUpdate {
        issue_identifier: issue_identifier.clone(),
        workspace_key: workspace_key.clone(),
        message: "Fetching origin".into(),
        at: now,
    };
    service
        .record_workspace_progress(update.clone())
        .await
        .unwrap();
    service.record_workspace_progress(update).await.unwrap();
    service
        .record_workspace_progress(WorkspaceProgressUpdate {
            issue_identifier,
            workspace_key,
            message: "Waiting for SSH key touch on github.com".into(),
            at: now + chrono::Duration::seconds(1),
        })
        .await
        .unwrap();
    service
        .record_workspace_progress(WorkspaceProgressUpdate {
            issue_identifier: "penso/arbor#89".into(),
            workspace_key: "penso_arbor_89".into(),
            message: "Waiting for SSH key touch on github.com".into(),
            at: now + chrono::Duration::seconds(2),
        })
        .await
        .unwrap();

    let tasks = service.state.tasks.get(&run_id).unwrap();
    assert_eq!(tasks[0].activity_log.len(), 2);
    assert!(tasks[0].activity_log[0].ends_with("Fetching origin"));
    assert!(tasks[0].activity_log[1].ends_with("Waiting for SSH key touch on github.com"));

    let snapshot = service.snapshot();
    let task_row = snapshot
        .tasks
        .iter()
        .find(|task| task.id == task_id)
        .unwrap();
    assert_eq!(task_row.activity_log.len(), 2);
    assert!(task_row.activity_log[0].ends_with("Fetching origin"));
}

#[tokio::test]
async fn task_retry_ignores_non_failed_tasks() {
    let workspace_root = unique_workspace_root("retry-only-failed");
    let provisioner = RecordingProvisioner::default();
    let mut service = test_service(TestTracker::new(Vec::new()), provisioner, &workspace_root);
    let now = Utc::now();
    let run_id = "run-retry-1".to_string();
    let task_id = "task-retry-1".to_string();

    service.state.runs.insert(run_id.clone(), Run {
        id: run_id.clone(),
        kind: RunKind::PullRequestReview,
        issue_id: Some("pr_review:github:penso/polyphony:89:head".into()),
        issue_identifier: Some("penso/polyphony#89".into()),
        title: "Retry me".into(),
        status: RunStatus::Failed,
        pipeline_stage: Some(PipelineStage::Executing),
        manual_dispatch_directives: None,
        workspace_key: Some("penso_polyphony_89".into()),
        workspace_path: Some(workspace_root.join("penso_polyphony_89")),
        review_target: None,
        deliverable: None,
        created_at: now,
        activity_log: Vec::new(),
        cancel_reason: None,
        blocked_outcome: None,
        steps: Vec::new(),
        updated_at: now,
    });
    service.state.tasks.insert(run_id.clone(), vec![Task {
        id: task_id.clone(),
        run_id: run_id.clone(),
        title: "Creating worktree".into(),
        description: None,
        activity_log: Vec::new(),
        category: polyphony_core::TaskCategory::Research,
        role: polyphony_core::PipelineTaskRole::Implementation,
        status: TaskStatus::Completed,
        ordinal: 0,
        parent_id: None,
        agent_name: Some("orchestrator".into()),
        session_id: None,
        thread_id: None,
        turns_completed: 0,
        tokens: TokenUsage::default(),
        started_at: Some(now),
        finished_at: Some(now),
        error: None,
        created_at: now,
        updated_at: now,
    }]);
    service
        .pending_task_retries
        .push((run_id.clone(), task_id.clone()));

    service.process_pending_task_retries().await;

    let task = service
        .state
        .tasks
        .get(&run_id)
        .and_then(|tasks| tasks.iter().find(|task| task.id == task_id))
        .expect("task should remain present");
    assert_eq!(task.status, TaskStatus::Completed);
    assert!(task.finished_at.is_some());
    assert!(
        service
            .state
            .recent_events
            .iter()
            .any(|event| event.message.contains("only failed tasks can retry")),
        "runtime should record why the retry was ignored"
    );
}

#[tokio::test]
async fn run_retry_relaunches_pull_request_review_from_first_failed_task() {
    let workspace_root = unique_workspace_root("retry-pr-run");
    let workflow = test_workflow_with_front_matter(
        &workspace_root,
        "---\ntracker:\n  kind: github\n  repository: penso/polyphony\n  api_key: token\npolling:\n  interval_ms: 1000\nworkspace:\n  root: __ROOT__\nagents:\n  default: reviewer\n  profiles:\n    reviewer:\n      kind: claude\n      transport: local_cli\n      command: claude -p --verbose --dangerously-skip-permissions\nreview_events:\n  pr_reviews:\n    enabled: true\n    agent: reviewer\n    debounce_seconds: 1\n---\nPrompt\n",
    );
    let (_tx, rx) = watch::channel(workflow.clone());
    let event = PullRequestReviewEvent {
        provider: polyphony_core::ReviewProviderKind::Github,
        repository: "penso/polyphony".into(),
        number: 89,
        title: "Retry me".into(),
        url: Some("https://github.com/penso/polyphony/pull/89".into()),
        base_branch: "main".into(),
        head_branch: "feature/retry".into(),
        head_sha: "abc123".into(),
        checkout_ref: Some("refs/pull/89/head".into()),
        author_login: Some("alice".into()),
        approval_state: DispatchApprovalState::Approved,
        labels: vec!["ready".into()],
        created_at: Some(Utc::now() - chrono::Duration::minutes(5)),
        updated_at: Some(Utc::now() - chrono::Duration::minutes(1)),
        is_draft: false,
    };
    let issue = synthetic_issue_for_pull_request_review(&event);
    let workspace_key = sanitize_workspace_key(&issue.identifier);
    let workspace_path = workspace_root.join(&workspace_key);
    let run_id = "run-retry-pr-1".to_string();
    let workspace_task_id = "task-retry-pr-setup".to_string();
    let review_task_id = "task-retry-pr-review".to_string();
    let now = Utc::now();

    let mut service = RuntimeService::new(
        Arc::new(TestTracker::new(Vec::new())),
        None,
        Arc::new(NoopAgent),
        Arc::new(RecordingProvisioner::default()),
        None,
        None,
        Some(Arc::new(RecordingPullRequestCommenter::default())),
        None,
        None,
        None,
        rx,
    )
    .0;

    service.state.runs.insert(run_id.clone(), Run {
        id: run_id.clone(),
        kind: RunKind::PullRequestReview,
        issue_id: Some(issue.id.clone()),
        issue_identifier: Some(issue.identifier.clone()),
        title: issue.title.clone(),
        status: RunStatus::Failed,
        pipeline_stage: None,
        manual_dispatch_directives: Some("Check auth".into()),
        workspace_key: Some(workspace_key.clone()),
        workspace_path: Some(workspace_path.clone()),
        review_target: Some(event.review_target()),
        deliverable: None,
        created_at: now,
        activity_log: Vec::new(),
        cancel_reason: None,
        blocked_outcome: None,
        steps: Vec::new(),
        updated_at: now,
    });
    service.state.tasks.insert(run_id.clone(), vec![
        Task {
            id: workspace_task_id.clone(),
            run_id: run_id.clone(),
            title: "Creating worktree".into(),
            description: None,
            activity_log: Vec::new(),
            category: polyphony_core::TaskCategory::Research,
            role: polyphony_core::PipelineTaskRole::Implementation,
            status: TaskStatus::Failed,
            ordinal: 0,
            parent_id: None,
            agent_name: Some("orchestrator".into()),
            session_id: None,
            thread_id: None,
            turns_completed: 0,
            tokens: TokenUsage::default(),
            started_at: Some(now),
            finished_at: Some(now),
            error: Some("auth failed".into()),
            created_at: now,
            updated_at: now,
        },
        Task {
            id: review_task_id.clone(),
            run_id: run_id.clone(),
            title: "Run PR review".into(),
            description: None,
            activity_log: Vec::new(),
            category: polyphony_core::TaskCategory::Review,
            role: polyphony_core::PipelineTaskRole::Implementation,
            status: TaskStatus::Cancelled,
            ordinal: 1,
            parent_id: None,
            agent_name: Some("reviewer".into()),
            session_id: None,
            thread_id: None,
            turns_completed: 0,
            tokens: TokenUsage::default(),
            started_at: None,
            finished_at: Some(now),
            error: Some("workspace setup failed".into()),
            created_at: now,
            updated_at: now,
        },
    ]);
    service
        .state
        .pull_request_retry_events
        .insert(issue.id.clone(), PullRequestEvent::Review(event.clone()));
    service.pending_run_retries.push(run_id.clone());

    service.process_pending_run_retries().await;

    let run = service
        .state
        .runs
        .get(&run_id)
        .expect("run should remain present");
    assert_eq!(run.status, RunStatus::InProgress);

    let tasks = service
        .state
        .tasks
        .get(&run_id)
        .expect("tasks should remain present");
    assert_eq!(tasks[0].status, TaskStatus::Completed);
    assert_eq!(tasks[0].error, None);
    assert_eq!(tasks[1].status, TaskStatus::InProgress);
    assert_eq!(tasks[1].error, None);

    let running = service
        .state
        .running
        .get(&issue.id)
        .expect("review worker should be relaunched");
    assert_eq!(
        running.active_task_id.as_deref(),
        Some(review_task_id.as_str())
    );
    assert_eq!(running.run_id.as_deref(), Some(run_id.as_str()));
    assert_eq!(running.issue.identifier, issue.identifier);
}

#[tokio::test]
async fn run_retry_recovers_stalled_pull_request_review_after_restart() {
    let workspace_root = unique_workspace_root("retry-pr-stalled");
    let workflow = test_workflow_with_front_matter(
        &workspace_root,
        "---\ntracker:\n  kind: github\n  repository: penso/polyphony\n  api_key: token\npolling:\n  interval_ms: 1000\nworkspace:\n  root: __ROOT__\nagents:\n  default: reviewer\n  profiles:\n    reviewer:\n      kind: claude\n      transport: local_cli\n      command: claude -p --verbose --dangerously-skip-permissions\nreview_events:\n  pr_reviews:\n    enabled: true\n    agent: reviewer\n    debounce_seconds: 1\n---\nPrompt\n",
    );
    let (_tx, rx) = watch::channel(workflow.clone());
    let event = PullRequestReviewEvent {
        provider: polyphony_core::ReviewProviderKind::Github,
        repository: "penso/polyphony".into(),
        number: 90,
        title: "Retry stale review".into(),
        url: Some("https://github.com/penso/polyphony/pull/90".into()),
        base_branch: "main".into(),
        head_branch: "feature/stalled".into(),
        head_sha: "def456".into(),
        checkout_ref: Some("refs/pull/90/head".into()),
        author_login: Some("alice".into()),
        approval_state: DispatchApprovalState::Approved,
        labels: vec!["ready".into()],
        created_at: Some(Utc::now() - chrono::Duration::minutes(5)),
        updated_at: Some(Utc::now() - chrono::Duration::minutes(1)),
        is_draft: false,
    };
    let issue = synthetic_issue_for_pull_request_review(&event);
    let workspace_key = sanitize_workspace_key(&issue.identifier);
    let workspace_path = workspace_root.join(&workspace_key);
    let run_id = "run-retry-pr-stalled".to_string();
    let workspace_task_id = "task-retry-pr-stalled-setup".to_string();
    let review_task_id = "task-retry-pr-stalled-review".to_string();
    let now = Utc::now();

    let mut service = RuntimeService::new(
        Arc::new(TestTracker::new(Vec::new())),
        None,
        Arc::new(NoopAgent),
        Arc::new(RecordingProvisioner::default()),
        None,
        None,
        Some(Arc::new(RecordingPullRequestCommenter::default())),
        None,
        None,
        None,
        rx,
    )
    .0;

    service.state.runs.insert(run_id.clone(), Run {
        id: run_id.clone(),
        kind: RunKind::PullRequestReview,
        issue_id: Some(issue.id.clone()),
        issue_identifier: Some(issue.identifier.clone()),
        title: issue.title.clone(),
        status: RunStatus::InProgress,
        pipeline_stage: None,
        manual_dispatch_directives: None,
        workspace_key: Some(workspace_key.clone()),
        workspace_path: Some(workspace_path.clone()),
        review_target: Some(event.review_target()),
        deliverable: None,
        created_at: now,
        activity_log: Vec::new(),
        cancel_reason: None,
        blocked_outcome: None,
        steps: Vec::new(),
        updated_at: now,
    });
    service.state.tasks.insert(run_id.clone(), vec![
        Task {
            id: workspace_task_id.clone(),
            run_id: run_id.clone(),
            title: "Creating worktree".into(),
            description: None,
            activity_log: Vec::new(),
            category: polyphony_core::TaskCategory::Research,
            role: polyphony_core::PipelineTaskRole::Implementation,
            status: TaskStatus::Pending,
            ordinal: 0,
            parent_id: None,
            agent_name: Some("orchestrator".into()),
            session_id: None,
            thread_id: None,
            turns_completed: 0,
            tokens: TokenUsage::default(),
            started_at: None,
            finished_at: None,
            error: None,
            created_at: now,
            updated_at: now,
        },
        Task {
            id: review_task_id.clone(),
            run_id: run_id.clone(),
            title: "Run PR review".into(),
            description: None,
            activity_log: Vec::new(),
            category: polyphony_core::TaskCategory::Review,
            role: polyphony_core::PipelineTaskRole::Implementation,
            status: TaskStatus::Cancelled,
            ordinal: 1,
            parent_id: None,
            agent_name: Some("reviewer".into()),
            session_id: None,
            thread_id: None,
            turns_completed: 0,
            tokens: TokenUsage::default(),
            started_at: None,
            finished_at: Some(now),
            error: Some("workspace setup failed".into()),
            created_at: now,
            updated_at: now,
        },
    ]);
    service
        .state
        .visible_review_events
        .insert(event.dedupe_key(), event.clone());
    service.pending_run_retries.push(run_id.clone());

    service.process_pending_run_retries().await;

    let tasks = service
        .state
        .tasks
        .get(&run_id)
        .expect("tasks should remain present");
    assert_eq!(tasks[0].status, TaskStatus::Completed);
    assert_eq!(tasks[1].status, TaskStatus::InProgress);
    assert!(
        service.state.running.contains_key(&issue.id),
        "stalled run retry should relaunch the review worker"
    );
}

#[tokio::test]
async fn manual_dispatch_with_agent_name() {
    let workspace_root = unique_workspace_root("manual-agent");
    let issue = sample_issue("issue-agent-1", "FAC-AGENT-1", "Todo", "Agent test");
    let tracker = TestTracker::new(vec![issue.clone()]);
    let mut service = test_service(tracker, RecordingProvisioner::default(), &workspace_root);

    service
        .pending_manual_dispatches
        .push(crate::ManualDispatchRequest {
            issue_id: issue.id.clone(),
            agent_name: Some("mock".into()),
            directives: None,
        });

    service.process_manual_dispatches().await;

    assert!(
        service.state.running.contains_key(&issue.id),
        "manual dispatch with explicit agent name should work"
    );
    let running = service.state.running.get(&issue.id).unwrap();
    assert_eq!(
        running.agent_name, "mock",
        "should use the explicitly requested agent"
    );
}

#[tokio::test]
async fn manual_dispatch_directives_are_prepended_to_direct_issue_prompt() {
    let workspace_root = unique_workspace_root("manual-directives-direct");
    let workflow = test_workflow(&workspace_root);
    let (_tx, rx) = watch::channel(workflow.clone());
    let issue = sample_issue(
        "issue-directives-1",
        "FAC-DIRECTIVES-1",
        "Todo",
        "Verify before fixing",
    );
    let tracker = TestTracker::new(vec![issue.clone()]);
    let agent = RecordingSessionAgent::default();
    let agent_handle = agent.clone();
    let mut service = RuntimeService::new(
        Arc::new(tracker),
        None,
        Arc::new(agent),
        Arc::new(RecordingProvisioner::default()),
        None,
        None,
        None,
        None,
        None,
        None,
        rx,
    )
    .0;
    let directives = "Please verify this is actually a bug before fixing it.";

    service
        .dispatch_issue(
            workflow,
            issue.clone(),
            None,
            false,
            None,
            false,
            Some(directives),
        )
        .await
        .unwrap();
    handle_next_worker_message(&mut service).await;

    let prompts = agent_handle.prompts();
    assert!(!prompts.is_empty());
    assert!(prompts[0].starts_with("## Operator Directives (Highest Priority)"));
    assert!(prompts[0].contains(directives));
    assert!(prompts[0].contains("Test prompt"));
    let run = service
        .state
        .runs
        .values()
        .find(|run| run.issue_id.as_deref() == Some(issue.id.as_str()))
        .expect("run should be created for direct dispatch");
    assert_eq!(run.manual_dispatch_directives.as_deref(), Some(directives));
}

#[tokio::test]
async fn manual_dispatch_directives_reach_pipeline_router_and_worker_prompts() {
    let workspace_root = unique_workspace_root("manual-directives-pipeline");
    let workflow = pipeline_workflow_with_automation(&workspace_root);
    let (_tx, rx) = watch::channel(workflow.clone());
    let issue = sample_issue(
        "issue-directives-2",
        "DOG-DIRECTIVES-2",
        "Todo",
        "Plan with operator guidance",
    );
    let tracker = TestTracker::new(vec![issue.clone()]);
    let agent = ScriptedPipelineAgent::default();
    let agent_handle = agent.clone();
    let mut service = RuntimeService::new(
        Arc::new(tracker),
        None,
        Arc::new(agent),
        Arc::new(RecordingProvisioner::default()),
        Some(Arc::new(RecordingCommitter::new(None))),
        Some(Arc::new(RecordingPullRequestManager::new(PullRequestRef {
            repository: "penso/polyphony".into(),
            number: 42,
            url: Some("https://github.com/penso/polyphony/pull/42".into()),
        }))),
        None,
        None,
        None,
        None,
        rx,
    )
    .0;
    let directives = "Please verify this is a bug first, then make a plan to fix it.";

    service
        .dispatch_issue(
            workflow,
            issue.clone(),
            None,
            false,
            None,
            false,
            Some(directives),
        )
        .await
        .unwrap();
    handle_next_worker_message(&mut service).await;
    handle_next_worker_message(&mut service).await;

    let calls = agent_handle.recorded_calls();
    let router_prompt = calls
        .iter()
        .find(|(agent_name, _)| agent_name == "router")
        .map(|(_, prompt)| prompt)
        .expect("router prompt should be recorded");
    assert!(router_prompt.starts_with("## Operator Directives (Highest Priority)"));
    assert!(router_prompt.contains(directives));

    let worker_prompt = calls
        .iter()
        .find(|(agent_name, prompt)| {
            agent_name == "implementer" && prompt.contains("## Pipeline Task")
        })
        .map(|(_, prompt)| prompt)
        .expect("pipeline worker prompt should be recorded");
    assert!(worker_prompt.contains(directives));

    let run = service
        .state
        .runs
        .values()
        .find(|run| run.issue_id.as_deref() == Some(issue.id.as_str()))
        .expect("run should be created for pipeline dispatch");
    assert_eq!(run.manual_dispatch_directives.as_deref(), Some(directives));
}

#[tokio::test]
async fn independent_qa_fixture_runs_implementation_qa_repair_and_fresh_qa_with_durable_evidence() {
    let workspace_root = unique_workspace_root("independent-qa-roles");
    let workflow = test_workflow_with_front_matter(
        &workspace_root,
        "---\ntracker:\n  kind: mock\npolling:\n  interval_ms: 1000\nworkspace:\n  root: __ROOT__\norchestration:\n  dispatch_mode: manual\nagents:\n  default: implementer\n  profiles:\n    implementer: { kind: mock, transport: mock, command: mock }\n    qa: { kind: mock, transport: mock, command: mock }\n    repair: { kind: mock, transport: mock, command: mock }\npipeline:\n  stages:\n    - { category: coding, role: implementation, agent: implementer }\n    - { category: review, role: qa, agent: qa }\n    - { category: coding, role: repair, agent: repair }\n    - { category: review, role: qa, agent: qa }\n---\nClosed-loop fixture\n",
    );
    let (_tx, rx) = watch::channel(workflow.clone());
    let mut issue = sample_issue(
        "issue-qa-fixture",
        "QA-17",
        "Todo",
        "Closed-loop QA fixture",
    );
    issue.description = Some("Acceptance checks\n1. implementation evidence\n2. qa pass evidence\n3. repair routing\n4. restart retention\n5. human-readable record".into());
    let agent = ClosedLoopQaFixtureAgent::default();
    let agent_handle = agent.clone();
    let tracker = TestTracker::new(vec![issue.clone()]);
    let mut service = RuntimeService::new(
        Arc::new(tracker.clone()),
        None,
        Arc::new(agent),
        Arc::new(RecordingProvisioner::default()),
        None,
        None,
        None,
        None,
        None,
        None,
        rx,
    )
    .0;

    service
        .dispatch_pipeline(workflow.clone(), issue.clone(), None, false, false, None)
        .await
        .unwrap();
    // Implementation and first QA (FAIL), then simulate a restart while the
    // distinct repair worker is active.
    for _ in 0..2 {
        handle_next_worker_message(&mut service).await;
    }
    let persisted_run: Run = serde_json::from_value(
        serde_json::to_value(service.state.runs.values().next().unwrap()).unwrap(),
    )
    .unwrap();
    let persisted_tasks: Vec<Task> = serde_json::from_value(
        serde_json::to_value(service.state.tasks[&persisted_run.id].clone()).unwrap(),
    )
    .unwrap();
    assert_eq!(persisted_tasks[1].status, TaskStatus::Failed);
    assert_eq!(persisted_tasks[2].status, TaskStatus::InProgress);
    assert!(
        persisted_tasks[1]
            .activity_log
            .iter()
            .any(|line| line.contains("QA FAIL") && line.contains("fixture found"))
    );

    let (_restart_tx, restart_rx) = watch::channel(workflow.clone());
    let mut restarted = RuntimeService::new(
        Arc::new(tracker.clone()),
        None,
        Arc::new(agent_handle.clone()),
        Arc::new(RecordingProvisioner::default()),
        None,
        None,
        None,
        None,
        None,
        None,
        restart_rx,
    )
    .0;
    restarted
        .state
        .runs
        .insert(persisted_run.id.clone(), persisted_run.clone());
    restarted
        .state
        .tasks
        .insert(persisted_run.id.clone(), persisted_tasks);
    restarted
        .normalize_restored_in_progress_runs()
        .await
        .unwrap();
    assert_eq!(
        restarted.state.tasks[&persisted_run.id][2].status,
        TaskStatus::Pending
    );
    assert_eq!(
        restarted.state.tasks[&persisted_run.id][1].status,
        TaskStatus::Failed
    );
    restarted
        .dispatch_next_task(
            restarted.workflow(),
            issue.clone(),
            None,
            false,
            &persisted_run.id,
            persisted_run.workspace_path.as_deref().unwrap(),
        )
        .await
        .unwrap();
    // Restart dispatches repair once, then fresh QA once; it never reruns the
    // first QA that supplied the persisted failure evidence.
    for _ in 0..2 {
        handle_next_worker_message(&mut restarted).await;
    }

    let calls = agent_handle.calls();
    // The original process may already have started its repair before this
    // in-memory crash simulation drops it. Crucially, recovery starts repair
    // (not QA) and produces exactly the original QA plus one fresh QA.
    assert_eq!(calls.iter().filter(|name| name.as_str() == "qa").count(), 2);
    assert_eq!(calls.last().map(String::as_str), Some("qa"));
    let run = restarted.state.runs.values().next().unwrap();
    let tasks = restarted.state.tasks.get(&run.id).unwrap();
    assert_eq!(
        tasks.iter().map(|task| task.role).collect::<Vec<_>>(),
        vec![
            polyphony_core::PipelineTaskRole::Implementation,
            polyphony_core::PipelineTaskRole::Qa,
            polyphony_core::PipelineTaskRole::Repair,
            polyphony_core::PipelineTaskRole::Qa,
        ]
    );
    assert_eq!(tasks[1].status, TaskStatus::Failed);
    assert!(
        tasks[1]
            .activity_log
            .iter()
            .any(|line| line.contains("QA FAIL") && line.contains("fixture found"))
    );
    assert_eq!(tasks[3].status, TaskStatus::Completed);
    assert!(
        tasks[3]
            .activity_log
            .iter()
            .any(|line| line.contains("QA PASS") && line.contains("fixture confirmed"))
    );

    // Task rows are the durable store representation.  The restarted fixture
    // retained both QA verdicts/evidence and has no pending duplicate QA.
    let restored: Vec<Task> = serde_json::from_value(serde_json::to_value(tasks).unwrap()).unwrap();
    assert_eq!(restored[1].activity_log, tasks[1].activity_log);
    assert_eq!(restored[3].activity_log, tasks[3].activity_log);
    assert!(
        restored
            .iter()
            .all(|task| task.status != TaskStatus::Pending)
    );
    let comments = tracker.recorded_comments();
    assert_eq!(
        comments.len(),
        4,
        "restart must retain rather than duplicate evidence notes"
    );
    assert!(
        comments
            .iter()
            .any(|note| note.body.contains("implementation note"))
    );
    assert!(
        comments
            .iter()
            .any(|note| note.body.contains("repair note"))
    );
    assert_eq!(
        comments
            .iter()
            .filter(|note| note.body.contains("QA note"))
            .count(),
        2
    );
    assert!(
        comments.iter().all(|note| {
            note.body.contains("tests run:")
                && note.body.contains("checks:")
                && note.body.contains("Role:")
                && note.body.contains("Task:")
        }),
        "tracker comments must retain a readable evidence checklist and role context"
    );
}

#[tokio::test]
async fn qa_failure_does_not_dispatch_repair_when_tracker_cannot_record_repair_needed() {
    let workspace_root = unique_workspace_root("qa-fail-closed-tracker-status");
    let workflow = test_workflow_with_front_matter(
        &workspace_root,
        "---\ntracker:\n  kind: mock\npolling:\n  interval_ms: 1000\nworkspace:\n  root: __ROOT__\norchestration:\n  dispatch_mode: manual\nagents:\n  default: implementer\n  profiles:\n    implementer: { kind: mock, transport: mock, command: mock }\n    qa: { kind: mock, transport: mock, command: mock }\n    repair: { kind: mock, transport: mock, command: mock }\npipeline:\n  stages:\n    - { category: review, role: qa, agent: qa }\n    - { category: coding, role: repair, agent: repair }\n---\nFail-closed QA fixture\n",
    );
    let (_tx, rx) = watch::channel(workflow.clone());
    let issue = sample_issue(
        "issue-qa-fail-closed",
        "QA-FAIL-CLOSED",
        "Todo",
        "QA failure",
    );
    let tracker = TestTracker::new(vec![issue.clone()])
        .fail_workflow_status_updates("simulated tracker status outage");
    let mut service = RuntimeService::new(
        Arc::new(tracker.clone()),
        None,
        Arc::new(NoopAgent),
        Arc::new(RecordingProvisioner::default()),
        None,
        None,
        None,
        None,
        None,
        None,
        rx,
    )
    .0;

    service
        .dispatch_pipeline(workflow.clone(), issue.clone(), None, false, false, None)
        .await
        .unwrap();
    let run_id = service.state.runs.keys().next().unwrap().clone();
    let task_id = service.state.tasks[&run_id][0].id.clone();
    service.state.running.remove(&issue.id);
    let failed_qa = AgentRunResult {
        status: AttemptStatus::Succeeded,
        turns_completed: 1,
        error: None,
        final_issue_state: Some(
            "QA FAIL: tracker status mutation must be durable\n\
            tests run: focused fixture\n\
            checks: 1\n\
            realistic: yes\n\
            material: yes\n\
            risks: lost evidence\n\
            small fix: yes\n\
            recommendation: remediate"
                .into(),
        ),
    };
    service
        .handle_task_finished(
            &workflow,
            &issue,
            &run_id,
            &task_id,
            &workspace_root,
            &failed_qa,
            None,
        )
        .await
        .unwrap();

    assert_eq!(service.state.runs[&run_id].status, RunStatus::Failed);
    assert_eq!(service.state.tasks[&run_id][0].status, TaskStatus::Failed);
    assert_eq!(service.state.tasks[&run_id][1].status, TaskStatus::Pending);
    assert!(
        !service.state.running.contains_key(&issue.id),
        "repair must not be dispatched after an unrecorded QA failure"
    );
    assert!(tracker.recorded_workflow_updates().is_empty());
    assert!(
        service.state.runs[&run_id]
            .activity_log
            .iter()
            .any(|log| log.message.contains("Repair Needed failed")
                && log.message.contains("not dispatched"))
    );
}

#[tokio::test]
async fn quality_bar_defers_exotic_non_material_hardening_and_allows_practical_acceptance() {
    let workspace_root = unique_workspace_root("quality-bar-defer");
    let workflow = test_workflow_with_front_matter(
        &workspace_root,
        "---\ntracker:\n  kind: mock\npolling:\n  interval_ms: 1000\nworkspace:\n  root: __ROOT__\norchestration:\n  dispatch_mode: manual\nagents:\n  default: qa\n  profiles:\n    qa: { kind: mock, transport: mock, command: mock }\n    repair: { kind: mock, transport: mock, command: mock }\npipeline:\n  stages:\n    - { category: review, role: qa, agent: qa }\n    - { category: coding, role: repair, agent: repair }\n---\nQuality-bar defer fixture\n",
    );
    let (_tx, rx) = watch::channel(workflow.clone());
    let mut issue = sample_issue(
        "issue-quality-bar-defer",
        "QA-DEFER",
        "Todo",
        "Defer hardening",
    );
    issue.description = Some("Acceptance checks\n1. quality gate".into());
    let tracker = TestTracker::new(vec![issue.clone()]);
    let mut service = RuntimeService::new(
        Arc::new(tracker.clone()),
        None,
        Arc::new(NoopAgent),
        Arc::new(RecordingProvisioner::default()),
        None,
        None,
        None,
        None,
        None,
        None,
        rx,
    )
    .0;

    service
        .dispatch_pipeline(workflow.clone(), issue.clone(), None, false, false, None)
        .await
        .unwrap();
    let run_id = service.state.runs.keys().next().unwrap().clone();
    let task_id = service.state.tasks[&run_id][0].id.clone();
    service.state.running.remove(&issue.id);
    let deferred_qa = AgentRunResult {
        status: AttemptStatus::Succeeded,
        turns_completed: 1,
        error: None,
        final_issue_state: Some(
            "QA FAIL: an exotic presentation variant is not material\n\
             tests run: quality-bar fixture\n\
             checks: 1\n\
             realistic: no\n\
             material: no\n\
             risks: none\n\
             small fix: no\n\
             recommendation: defer\n\
             follow-up: #21"
                .into(),
        ),
    };
    service
        .handle_task_finished(
            &workflow,
            &issue,
            &run_id,
            &task_id,
            &workspace_root,
            &deferred_qa,
            None,
        )
        .await
        .unwrap();

    assert_eq!(service.state.runs[&run_id].status, RunStatus::Delivered);
    assert_eq!(service.state.tasks[&run_id][0].status, TaskStatus::Failed);
    assert_eq!(
        service.state.tasks[&run_id][1].status,
        TaskStatus::Cancelled
    );
    assert!(
        !tracker
            .recorded_workflow_updates()
            .iter()
            .any(|status| status == "Repair Needed")
    );
    assert!(
        service.state.runs[&run_id]
            .activity_log
            .iter()
            .any(|entry| {
                entry.message.contains("recommendation=defer")
                    && entry.message.contains("follow_up=#21")
            })
    );
    assert!(tracker.recorded_comments().iter().any(|comment| {
        comment.body.contains("recommendation: defer") && comment.body.contains("follow-up: #21")
    }));
}

#[tokio::test]
async fn quality_bar_requires_human_decision_for_non_material_lifecycle_risks() {
    let workspace_root = unique_workspace_root("quality-bar-non-material-risk");
    let workflow = test_workflow_with_front_matter(
        &workspace_root,
        "---\ntracker:\n  kind: mock\npolling:\n  interval_ms: 1000\nworkspace:\n  root: __ROOT__\norchestration:\n  dispatch_mode: manual\nagents:\n  default: qa\n  profiles:\n    qa: { kind: mock, transport: mock, command: mock }\n    repair: { kind: mock, transport: mock, command: mock }\npipeline:\n  stages:\n    - { category: review, role: qa, agent: qa }\n    - { category: coding, role: repair, agent: repair }\n---\nQuality-bar non-material risk fixture\n",
    );
    let (_tx, rx) = watch::channel(workflow.clone());
    let mut issue = sample_issue(
        "issue-quality-bar-non-material-risk",
        "QA-NON-MATERIAL-RISK",
        "Todo",
        "Non-material lifecycle risk",
    );
    issue.description = Some("Acceptance checks\n1. quality gate".into());
    let tracker = TestTracker::new(vec![issue.clone()]);
    let mut service = RuntimeService::new(
        Arc::new(tracker.clone()),
        None,
        Arc::new(NoopAgent),
        Arc::new(RecordingProvisioner::default()),
        None,
        None,
        None,
        None,
        None,
        None,
        rx,
    )
    .0;

    service
        .dispatch_pipeline(workflow.clone(), issue.clone(), None, false, false, None)
        .await
        .unwrap();
    let run_id = service.state.runs.keys().next().unwrap().clone();
    let task_id = service.state.tasks[&run_id][0].id.clone();
    service.state.running.remove(&issue.id);
    let qa_needing_human = AgentRunResult {
        status: AttemptStatus::Succeeded,
        turns_completed: 1,
        error: None,
        final_issue_state: Some(
            "QA FAIL: non-material lifecycle risks need a human decision\n\
             tests run: quality-bar fixture\n\
             checks: 1\n\
             realistic: yes\n\
             material: no\n\
             risks: false pass, lost evidence, duplicate work, human-control bypass\n\
             small fix: yes\n\
             recommendation: needs human decision"
                .into(),
        ),
    };
    service
        .handle_task_finished(
            &workflow,
            &issue,
            &run_id,
            &task_id,
            &workspace_root,
            &qa_needing_human,
            None,
        )
        .await
        .unwrap();

    assert_eq!(service.state.runs[&run_id].status, RunStatus::Review);
    assert_eq!(service.state.tasks[&run_id][0].status, TaskStatus::Failed);
    assert_eq!(service.state.tasks[&run_id][1].status, TaskStatus::Pending);
    assert!(
        !service.state.running.contains_key(&issue.id),
        "a non-material finding must not dispatch repair automatically"
    );
    assert!(
        !tracker
            .recorded_workflow_updates()
            .iter()
            .any(|status| status == "Repair Needed")
    );
    assert!(
        service.state.runs[&run_id]
            .activity_log
            .iter()
            .any(|entry| {
                entry.message.contains("material=false")
                    && entry.message.contains("decision=needs human decision")
            })
    );
    assert!(tracker.recorded_comments().iter().any(|comment| {
        comment.body.contains("material: no")
            && comment
                .body
                .contains("recommendation: needs human decision")
    }));
}

#[tokio::test]
async fn quality_bar_rejects_contradictory_or_high_risk_deferral_without_dispatching_repair() {
    let workspace_root = unique_workspace_root("quality-bar-contradiction");
    let workflow = test_workflow_with_front_matter(
        &workspace_root,
        "---\ntracker:\n  kind: mock\npolling:\n  interval_ms: 1000\nworkspace:\n  root: __ROOT__\norchestration:\n  dispatch_mode: manual\nagents:\n  default: qa\n  profiles:\n    qa: { kind: mock, transport: mock, command: mock }\n    repair: { kind: mock, transport: mock, command: mock }\npipeline:\n  stages:\n    - { category: review, role: qa, agent: qa }\n    - { category: coding, role: repair, agent: repair }\n---\nQuality-bar contradiction fixture\n",
    );
    let (_tx, rx) = watch::channel(workflow.clone());
    let mut issue = sample_issue(
        "issue-quality-bar-contradiction",
        "QA-CONTRADICT",
        "Todo",
        "Contradiction",
    );
    issue.description = Some("Acceptance checks\n1. quality gate".into());
    let tracker = TestTracker::new(vec![issue.clone()]);
    let mut service = RuntimeService::new(
        Arc::new(tracker.clone()),
        None,
        Arc::new(NoopAgent),
        Arc::new(RecordingProvisioner::default()),
        None,
        None,
        None,
        None,
        None,
        None,
        rx,
    )
    .0;

    service
        .dispatch_pipeline(workflow.clone(), issue.clone(), None, false, false, None)
        .await
        .unwrap();
    let run_id = service.state.runs.keys().next().unwrap().clone();
    let task_id = service.state.tasks[&run_id][0].id.clone();
    service.state.running.remove(&issue.id);
    let contradictory_qa = AgentRunResult {
        status: AttemptStatus::Succeeded,
        turns_completed: 1,
        error: None,
        final_issue_state: Some(
            "QA FAIL: a possible false PASS must not be deferred\n\
             tests run: quality-bar fixture\n\
             checks: 1\n\
             realistic: yes\n\
             material: yes\n\
             risks: false pass\n\
             small fix: yes\n\
             recommendation: defer\n\
             follow-up: #22"
                .into(),
        ),
    };
    service
        .handle_task_finished(
            &workflow,
            &issue,
            &run_id,
            &task_id,
            &workspace_root,
            &contradictory_qa,
            None,
        )
        .await
        .unwrap();

    assert_eq!(service.state.runs[&run_id].status, RunStatus::Failed);
    assert_eq!(service.state.tasks[&run_id][1].status, TaskStatus::Pending);
    assert!(!service.state.running.contains_key(&issue.id));
    assert!(
        !tracker
            .recorded_workflow_updates()
            .iter()
            .any(|status| status == "Repair Needed")
    );
    assert!(
        service.state.runs[&run_id]
            .activity_log
            .iter()
            .any(|entry| {
                entry
                    .message
                    .contains("without a valid durable quality-bar assessment")
            })
    );
}

#[tokio::test]
async fn quality_bar_human_override_is_durable_and_restart_resumes_only_repair() {
    let workspace_root = unique_workspace_root("quality-bar-override-restart");
    let workflow = test_workflow_with_front_matter(
        &workspace_root,
        "---\ntracker:\n  kind: mock\npolling:\n  interval_ms: 1000\nworkspace:\n  root: __ROOT__\norchestration:\n  dispatch_mode: manual\nagents:\n  default: qa\n  profiles:\n    qa: { kind: mock, transport: mock, command: mock }\n    repair: { kind: mock, transport: mock, command: mock }\npipeline:\n  stages:\n    - { category: review, role: qa, agent: qa }\n    - { category: coding, role: repair, agent: repair }\n---\nQuality-bar override fixture\n",
    );
    let (_tx, rx) = watch::channel(workflow.clone());
    let mut issue = sample_issue(
        "issue-quality-bar-override",
        "QA-OVERRIDE",
        "Todo",
        "Human override",
    );
    issue.description = Some("Acceptance checks\n1. quality gate".into());
    issue.comments.push(IssueComment {
        id: "human-override".into(),
        body: "QUALITY BAR OVERRIDE: remediate".into(),
        author: Some(IssueAuthor {
            id: None,
            username: None,
            display_name: Some("Owner".into()),
            role: Some("owner".into()),
            trust_level: Some("trusted_owner".into()),
            url: None,
        }),
        url: Some("https://tracker.test/override".into()),
        created_at: Some(Utc::now()),
        updated_at: None,
    });
    let tracker = TestTracker::new(vec![issue.clone()]);
    let mut service = RuntimeService::new(
        Arc::new(tracker.clone()),
        None,
        Arc::new(NoopAgent),
        Arc::new(RecordingProvisioner::default()),
        None,
        None,
        None,
        None,
        None,
        None,
        rx,
    )
    .0;

    service
        .dispatch_pipeline(workflow.clone(), issue.clone(), None, false, false, None)
        .await
        .unwrap();
    let run_id = service.state.runs.keys().next().unwrap().clone();
    let task_id = service.state.tasks[&run_id][0].id.clone();
    service.state.running.remove(&issue.id);
    let qa_needing_human = AgentRunResult {
        status: AttemptStatus::Succeeded,
        turns_completed: 1,
        error: None,
        final_issue_state: Some(
            "QA FAIL: a material case has no small bounded fix\n\
             tests run: quality-bar fixture\n\
             checks: 1\n\
             realistic: yes\n\
             material: yes\n\
             risks: none\n\
             small fix: no\n\
             recommendation: needs human decision"
                .into(),
        ),
    };
    service
        .handle_task_finished(
            &workflow,
            &issue,
            &run_id,
            &task_id,
            &workspace_root,
            &qa_needing_human,
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        service.state.tasks[&run_id][1].status,
        TaskStatus::InProgress
    );
    assert!(
        service.state.runs[&run_id]
            .activity_log
            .iter()
            .any(|entry| {
                entry.message.contains("human_override=remediate")
                    && entry.message.contains("decision=remediate")
            })
    );

    let persisted_run: Run =
        serde_json::from_value(serde_json::to_value(service.state.runs[&run_id].clone()).unwrap())
            .unwrap();
    let persisted_tasks: Vec<Task> =
        serde_json::from_value(serde_json::to_value(service.state.tasks[&run_id].clone()).unwrap())
            .unwrap();
    let (_restart_tx, restart_rx) = watch::channel(workflow.clone());
    let mut restarted = RuntimeService::new(
        Arc::new(tracker),
        None,
        Arc::new(NoopAgent),
        Arc::new(RecordingProvisioner::default()),
        None,
        None,
        None,
        None,
        None,
        None,
        restart_rx,
    )
    .0;
    restarted.state.runs.insert(run_id.clone(), persisted_run);
    restarted
        .state
        .tasks
        .insert(run_id.clone(), persisted_tasks);
    restarted
        .normalize_restored_in_progress_runs()
        .await
        .unwrap();
    assert_eq!(
        restarted.state.tasks[&run_id][1].status,
        TaskStatus::Pending
    );
    assert_eq!(restarted.state.tasks[&run_id][0].status, TaskStatus::Failed);
    assert!(
        restarted.state.runs[&run_id]
            .activity_log
            .iter()
            .any(|entry| { entry.message.contains("human_override=remediate") })
    );
}

#[tokio::test]
async fn restart_crash_window_reconciles_existing_evidence_marker_without_duplicate_comment() {
    let workspace_root = unique_workspace_root("evidence-publication-reconciliation");
    let workflow = test_workflow_with_front_matter(
        &workspace_root,
        "---\ntracker:\n  kind: mock\npolling:\n  interval_ms: 1000\nworkspace:\n  root: __ROOT__\norchestration:\n  dispatch_mode: manual\nagents:\n  default: implementer\n  profiles:\n    implementer: { kind: mock, transport: mock, command: mock }\n    qa: { kind: mock, transport: mock, command: mock }\npipeline:\n  stages:\n    - { category: coding, role: implementation, agent: implementer }\n    - { category: review, role: qa, agent: qa }\n---\nEvidence reconciliation fixture\n",
    );
    let (_tx, rx) = watch::channel(workflow.clone());
    let issue = sample_issue(
        "issue-evidence-reconcile",
        "EVIDENCE-RECONCILE",
        "Todo",
        "Publication recovery",
    );
    let tracker = TestTracker::new(vec![issue.clone()]);
    let mut service = RuntimeService::new(
        Arc::new(tracker.clone()),
        None,
        Arc::new(NoopAgent),
        Arc::new(RecordingProvisioner::default()),
        None,
        None,
        None,
        None,
        None,
        None,
        rx,
    )
    .0;

    service
        .dispatch_pipeline(workflow.clone(), issue.clone(), None, false, false, None)
        .await
        .unwrap();
    let run_id = service.state.runs.keys().next().unwrap().clone();
    let task_id = service.state.tasks[&run_id][0].id.clone();
    service.state.running.remove(&issue.id);
    // Simulate a crash after the tracker accepted this comment but before the
    // task/run checkpoint could be persisted.  A restarted tracker fetch
    // supplies the existing comment in the issue snapshot.
    let completed = AgentRunResult {
        status: AttemptStatus::Succeeded,
        turns_completed: 1,
        error: None,
        final_issue_state: Some(
            "IMPLEMENTATION NOTE: tracker note survived crash\n\
            what changed: verified the crash-window tracker marker\n\
            commit: none — no code change\n\
            tests run: focused fixture\n\
            checks: 1"
                .into(),
        ),
    };
    let task = service.state.tasks[&run_id][0].clone();
    let note = RuntimeService::delivery_note(&task, &issue, &completed).unwrap();
    let mut restarted_issue = issue.clone();
    restarted_issue.comments.push(IssueComment {
        id: "already-published".into(),
        body: RuntimeService::delivery_comment_body(&run_id, &task, &note),
        author: None,
        url: None,
        created_at: None,
        updated_at: None,
    });
    service
        .handle_task_finished(
            &workflow,
            &restarted_issue,
            &run_id,
            &task_id,
            &workspace_root,
            &completed,
            None,
        )
        .await
        .unwrap();

    assert_eq!(
        service.state.tasks[&run_id][0].status,
        TaskStatus::Completed
    );
    assert!(
        tracker.recorded_comments().is_empty(),
        "existing marker must prevent a duplicate evidence comment"
    );
    assert!(
        service.state.runs[&run_id]
            .activity_log
            .iter()
            .any(|log| log.message.contains("not posting a duplicate"))
    );
}

#[tokio::test]
async fn evidence_reconciliation_rejects_marker_bypasses_and_only_accepts_one_complete_matching_note()
 {
    let workspace_root = unique_workspace_root("evidence-marker-adversarial");
    let workflow = test_workflow_with_front_matter(
        &workspace_root,
        "---\ntracker:\n  kind: mock\npolling:\n  interval_ms: 1000\nworkspace:\n  root: __ROOT__\norchestration:\n  dispatch_mode: manual\nagents:\n  default: implementer\n  profiles:\n    implementer: { kind: mock, transport: mock, command: mock }\n    qa: { kind: mock, transport: mock, command: mock }\npipeline:\n  stages:\n    - { category: coding, role: implementation, agent: implementer }\n    - { category: review, role: qa, agent: qa }\n---\nEvidence fixture\n",
    );
    let (_tx, rx) = watch::channel(workflow.clone());
    let issue = sample_issue(
        "issue-marker-adversarial",
        "EV-MARKER",
        "Todo",
        "Marker safety",
    );
    let tracker = TestTracker::new(vec![issue.clone()]);
    let mut service = RuntimeService::new(
        Arc::new(tracker.clone()),
        None,
        Arc::new(NoopAgent),
        Arc::new(RecordingProvisioner::default()),
        None,
        None,
        None,
        None,
        None,
        None,
        rx,
    )
    .0;
    service
        .dispatch_pipeline(workflow.clone(), issue.clone(), None, false, false, None)
        .await
        .unwrap();
    let run_id = service.state.runs.keys().next().unwrap().clone();
    let task = service.state.tasks[&run_id][0].clone();
    let outcome = AgentRunResult {
        status: AttemptStatus::Succeeded, turns_completed: 1, error: None,
        final_issue_state: Some("IMPLEMENTATION NOTE: evidence\nwhat changed: added the guard\ncommit: abc123\ntests run: focused test\nchecks: 1".into()),
    };
    let note = RuntimeService::delivery_note(&task, &issue, &outcome).unwrap();
    let canonical_marker = RuntimeService::delivery_marker(&run_id, &task, &note);

    // Legacy, substring, and duplicate legacy markers are not canonical v2
    // evidence and therefore cannot suppress publication of the valid note.
    let mut marker_bypass_issue = issue.clone();
    marker_bypass_issue.comments = [
        "<!-- polyphony:delivery-evidence run=forged task=forged -->",
        "prefix <!-- polyphony:delivery-evidence run=forged task=forged --> suffix",
        "<!-- polyphony:delivery-evidence run=forged task=forged -->",
    ]
    .into_iter()
    .enumerate()
    .map(|(index, body)| IssueComment {
        id: format!("forged-{index}"),
        body: body.into(),
        author: None,
        url: None,
        created_at: None,
        updated_at: None,
    })
    .collect();
    service
        .publish_delivery_note(&marker_bypass_issue, &run_id, &task, &note)
        .await
        .unwrap();
    assert_eq!(
        tracker.recorded_comments().len(),
        1,
        "forged markers must not suppress valid evidence"
    );

    // A current marker has an exact run/task/role/evidence identity.  A
    // marker-only or conflicting use of that identity is ambiguous, so it
    // fails closed instead of completing the task or posting a second valid note.
    for comments in [
        vec![canonical_marker.clone()],
        vec![
            RuntimeService::delivery_comment_body(&run_id, &task, &note),
            RuntimeService::delivery_comment_body(&run_id, &task, &note),
        ],
        vec![format!(
            "{canonical_marker}\n## Polyphony implementation note\n\nwrong evidence"
        )],
    ] {
        let mut conflicting_issue = issue.clone();
        conflicting_issue.comments = comments
            .into_iter()
            .enumerate()
            .map(|(index, body)| IssueComment {
                id: format!("conflict-{index}"),
                body,
                author: None,
                url: None,
                created_at: None,
                updated_at: None,
            })
            .collect();
        assert!(
            service
                .publish_delivery_note(&conflicting_issue, &run_id, &task, &note)
                .await
                .is_err()
        );
    }
    assert_eq!(
        tracker.recorded_comments().len(),
        1,
        "ambiguous canonical markers must not create duplicates"
    );

    let mut marker_only_issue = issue.clone();
    marker_only_issue.comments.push(IssueComment {
        id: "marker-only".into(),
        body: canonical_marker,
        author: None,
        url: None,
        created_at: None,
        updated_at: None,
    });
    assert!(
        service
            .handle_task_finished(
                &workflow,
                &marker_only_issue,
                &run_id,
                &task.id,
                &workspace_root,
                &outcome,
                None,
            )
            .await
            .is_err()
    );
    assert_ne!(
        service.state.tasks[&run_id][0].status,
        TaskStatus::Completed,
        "a marker-only comment must not complete a task without valid durable evidence"
    );
}

#[tokio::test]
async fn closed_loop_implementation_cannot_complete_without_an_implementation_note() {
    let workspace_root = unique_workspace_root("implementation-note-required");
    let workflow = test_workflow_with_front_matter(
        &workspace_root,
        "---\ntracker:\n  kind: mock\npolling:\n  interval_ms: 1000\nworkspace:\n  root: __ROOT__\norchestration:\n  dispatch_mode: manual\nagents:\n  default: implementer\n  profiles:\n    implementer: { kind: mock, transport: mock, command: mock }\n    qa: { kind: mock, transport: mock, command: mock }\npipeline:\n  stages:\n    - { category: coding, role: implementation, agent: implementer }\n    - { category: review, role: qa, agent: qa }\n---\nEvidence fixture\n",
    );
    let (_tx, rx) = watch::channel(workflow.clone());
    let issue = sample_issue(
        "issue-implementation-note",
        "EV-1",
        "Todo",
        "Evidence required",
    );
    let mut service = RuntimeService::new(
        Arc::new(TestTracker::new(vec![issue.clone()])),
        None,
        Arc::new(NoopAgent),
        Arc::new(RecordingProvisioner::default()),
        None,
        None,
        None,
        None,
        None,
        None,
        rx,
    )
    .0;
    service
        .dispatch_pipeline(workflow.clone(), issue.clone(), None, false, false, None)
        .await
        .unwrap();
    let run_id = service.state.runs.keys().next().unwrap().clone();
    let task_id = service.state.tasks[&run_id][0].id.clone();
    service.state.running.remove(&issue.id);
    service
        .handle_task_finished(
            &workflow,
            &issue,
            &run_id,
            &task_id,
            &workspace_root,
            &AgentRunResult::succeeded(1),
            None,
        )
        .await
        .unwrap();
    assert_eq!(service.state.tasks[&run_id][0].status, TaskStatus::Failed);
    assert_eq!(service.state.runs[&run_id].status, RunStatus::Failed);
    assert!(
        service.state.tasks[&run_id][0]
            .error
            .as_deref()
            .unwrap()
            .contains("IMPLEMENTATION NOTE")
    );
}

#[test]
fn delivery_evidence_rejects_fake_workers_that_omit_each_required_checklist_field() {
    let issue = Issue {
        description: Some("Acceptance checks\n1. first\n2. second".into()),
        ..sample_issue(
            "issue-checklist-fields",
            "EV-FIELDS",
            "Todo",
            "Checklist fields",
        )
    };
    let now = Utc::now();
    let task = |role| Task {
        id: "task-checklist-fields".into(),
        run_id: "run-checklist-fields".into(),
        title: "Checklist evidence".into(),
        description: None,
        activity_log: Vec::new(),
        category: polyphony_core::TaskCategory::Coding,
        role,
        status: TaskStatus::InProgress,
        ordinal: 1,
        parent_id: None,
        agent_name: None,
        session_id: None,
        thread_id: None,
        turns_completed: 0,
        tokens: TokenUsage::default(),
        started_at: None,
        finished_at: None,
        error: None,
        created_at: now,
        updated_at: now,
    };
    let outcome = |note: &str| AgentRunResult {
        status: AttemptStatus::Succeeded,
        turns_completed: 1,
        error: None,
        final_issue_state: Some(note.into()),
    };
    let cases = [
        (
            polyphony_core::PipelineTaskRole::Implementation,
            "IMPLEMENTATION NOTE:\nwhat changed: added a guard\ncommit: abc123\ntests run: cargo test\nchecks: 1, 2",
            &["what changed", "commit", "tests run", "checks"][..],
        ),
        (
            polyphony_core::PipelineTaskRole::Repair,
            "REPAIR NOTE:\nwhat fixed: corrected the guard\ncommit: def456\ntests run: cargo test\nrecheck: QA checks 1 and 2\nchecks: 1, 2",
            &["what fixed", "commit", "tests run", "recheck", "checks"][..],
        ),
        (
            polyphony_core::PipelineTaskRole::Qa,
            "QA PASS: all checks pass\ntests run: cargo test\nchecks: 1, 2",
            &["tests run", "checks"][..],
        ),
    ];

    for (role, complete_note, fields) in cases {
        assert!(
            RuntimeService::delivery_note(&task(role), &issue, &outcome(complete_note)).is_ok()
        );
        for field in fields {
            let incomplete = complete_note
                .lines()
                .filter(|line| !line.starts_with(&format!("{field}:")))
                .collect::<Vec<_>>()
                .join("\n");
            let error = RuntimeService::delivery_note(&task(role), &issue, &outcome(&incomplete))
                .expect_err("fake worker omission must fail closed");
            assert!(error.contains(field), "expected {field} in {error}");

            let duplicated = format!("{complete_note}\n{field}: conflicting duplicate");
            let error = RuntimeService::delivery_note(&task(role), &issue, &outcome(&duplicated))
                .expect_err("duplicate structured evidence must fail closed");
            assert!(error.contains(field), "expected {field} in {error}");
        }
    }

    // A legitimate non-code result remains possible, but has to say so in
    // the durable record rather than silently omitting its commit evidence.
    let no_commit = "IMPLEMENTATION NOTE:\nwhat changed: documented the decision\ncommit: none — no code change\ntests run: not applicable\nchecks: 1";
    assert!(
        RuntimeService::delivery_note(
            &task(polyphony_core::PipelineTaskRole::Implementation),
            &issue,
            &outcome(no_commit)
        )
        .is_ok()
    );

    // Both coding roles may have a legitimate non-code outcome, but the
    // durable note must make its reason explicit.  Do not accept bare or
    // ambiguous `none` values as if they were commit identifiers.
    let repair_no_commit = "REPAIR NOTE:\nwhat fixed: documented the decision\ncommit: none — no code change\ntests run: not applicable\nrecheck: QA checks 1\nchecks: 1";
    assert!(
        RuntimeService::delivery_note(
            &task(polyphony_core::PipelineTaskRole::Repair),
            &issue,
            &outcome(repair_no_commit)
        )
        .is_ok()
    );
    for (role, valid_no_commit) in [
        (polyphony_core::PipelineTaskRole::Implementation, no_commit),
        (polyphony_core::PipelineTaskRole::Repair, repair_no_commit),
    ] {
        for invalid_commit in [
            "none",
            "none —",
            "none —    ",
            "none - no code change",
            "not-a-commit",
        ] {
            let invalid_note = valid_no_commit.replace("none — no code change", invalid_commit);
            let error = RuntimeService::delivery_note(&task(role), &issue, &outcome(&invalid_note))
                .expect_err("invalid no-commit evidence must fail closed");
            assert!(error.contains("commit"), "expected commit error in {error}");
        }

        let conflicting_duplicate =
            valid_no_commit.replace("none — no code change", "abc123\ncommit: none");
        let error =
            RuntimeService::delivery_note(&task(role), &issue, &outcome(&conflicting_duplicate))
                .expect_err("duplicate commit evidence must fail closed");
        assert!(error.contains("commit"), "expected commit error in {error}");
    }

    let qa_task = task(polyphony_core::PipelineTaskRole::Qa);
    let qa_note = "QA PASS: all checks pass\ntests run: cargo test\nchecks: 1, 2";
    for malformed in [
        "checks: 1 2",
        "checks: 1; 2",
        "checks: 1, 1",
        "checks: 01, 2",
        "checks: 1, 2oops",
        "checks: 1, 2, 3",
        "checks: 1",
    ] {
        let error = RuntimeService::delivery_note(
            &qa_task,
            &issue,
            &outcome(&qa_note.replace("checks: 1, 2", malformed)),
        )
        .expect_err("malformed or non-exact QA coverage must fail closed");
        assert!(
            error.contains("checks") || error.contains("acceptance"),
            "expected check coverage error in {error}"
        );
    }
    for variant in [
        "Checks: 1, 2",
        " checks: 1, 2",
        "checks : 1, 2",
        "checks\u{00a0}: 1, 2",
    ] {
        let error = RuntimeService::delivery_note(
            &qa_task,
            &issue,
            &outcome(&qa_note.replace("checks: 1, 2", variant)),
        )
        .expect_err("case and whitespace key variants must fail closed");
        assert!(error.contains("checks"), "expected checks error in {error}");
    }
    let malformed_acceptance = Issue {
        description: Some("Acceptance checks\n01. first\n2. second".into()),
        ..issue.clone()
    };
    assert!(
        RuntimeService::delivery_note(&qa_task, &malformed_acceptance, &outcome(qa_note))
            .expect_err("noncanonical acceptance numbering must fail closed")
            .contains("acceptance check")
    );
}

#[test]
fn delivery_evidence_uses_bounded_canonical_check_ids_and_visible_unicode_values() {
    let issue = Issue {
        description: Some("Acceptance checks\n1. first\n2. second".into()),
        ..sample_issue(
            "issue-evidence-grammar-boundaries",
            "EV-GRAMMAR",
            "Todo",
            "Evidence grammar boundaries",
        )
    };
    let now = Utc::now();
    let task = |role| Task {
        id: "task-evidence-grammar-boundaries".into(),
        run_id: "run-evidence-grammar-boundaries".into(),
        title: "Checklist evidence".into(),
        description: None,
        activity_log: Vec::new(),
        category: polyphony_core::TaskCategory::Coding,
        role,
        status: TaskStatus::InProgress,
        ordinal: 1,
        parent_id: None,
        agent_name: None,
        session_id: None,
        thread_id: None,
        turns_completed: 0,
        tokens: TokenUsage::default(),
        started_at: None,
        finished_at: None,
        error: None,
        created_at: now,
        updated_at: now,
    };
    let outcome = |note: &str| AgentRunResult {
        status: AttemptStatus::Succeeded,
        turns_completed: 1,
        error: None,
        final_issue_state: Some(note.into()),
    };
    let qa_task = task(polyphony_core::PipelineTaskRole::Qa);

    // The acceptance list is protocol input.  A line that looks numbered but
    // is malformed is never ignored, because doing so would make QA coverage
    // vacuous when no valid check lines remain.
    for description in [
        "Acceptance checks\n+1. signed",
        "Acceptance checks\n-1. signed",
        "Acceptance checks\n- 1. unordered Markdown wrapper",
        "Acceptance checks\n* 1. unordered Markdown wrapper",
        "Acceptance checks\n## 1. heading wrapper",
        "Acceptance checks\n1) Markdown ordered-list variant",
        "Acceptance checks\n1 . whitespace-obscured separator",
        "Acceptance checks\n01. leading zero",
        "Acceptance checks\n0. zero",
        "Acceptance checks\n1000. out of configured range",
        "Acceptance checks\n999999999999999999999999999999999999999. overflow",
        "Acceptance checks\n1. first\n1. duplicate",
        "Acceptance checks\n2. second\n1. first",
        "Acceptance checks\n١. non-ASCII numeral",
        "Acceptance checks\n１. full-width numeral",
        "Acceptance checks\n−1. Unicode minus",
        "Acceptance checks\n＋1. Unicode plus",
        "Acceptance checks\n﹢1. small Unicode plus",
        "Acceptance checks\n-\u{00a0}1. Unicode whitespace after sign",
        "Acceptance checks\n1\u{200b}. zero-width-obscured separator",
        "Acceptance checks\n1\u{200b}2. zero-width-obscured digits",
        "Acceptance checks\n\u{202e}1. bidi-obscured marker",
        "Acceptance checks\n1. first\n- 2. mixed malformed Markdown item",
    ] {
        let malformed = Issue {
            description: Some(description.into()),
            ..issue.clone()
        };
        let error = RuntimeService::delivery_note(
            &qa_task,
            &malformed,
            &outcome("QA PASS: false pass attempt\ntests run: cargo test\nchecks: arbitrary text"),
        )
        .expect_err("malformed acceptance input must fail before QA coverage can pass");
        assert!(
            error.contains("acceptance check"),
            "expected acceptance-list error in {error}"
        );
    }

    // The grammar is scoped to an explicit acceptance heading.  Outside that
    // section, natural number-leading prose is not protocol input.  Within it,
    // unnumbered prose is still allowed, but an empty section cannot make QA
    // coverage vacuous.
    let prose_and_heading_boundary = Issue {
        description: Some(
            "2026 roadmap\n\
             ## Acceptance criteria\n\
             This is explanatory prose.\n\
             - ordinary unnumbered Markdown prose\n\
             1. first\n\
             2. second\n\
             ## Background\n\
             2027 roadmap\n\
             3. not an acceptance check"
                .into(),
        ),
        ..issue.clone()
    };
    assert!(
        RuntimeService::delivery_note(
            &qa_task,
            &prose_and_heading_boundary,
            &outcome("QA PASS: scoped checks\ntests run: cargo test\nchecks: 1, 2"),
        )
        .is_ok(),
        "ordinary prose and later sections must not become accidental checks"
    );
    for description in [
        "Acceptance checks\n- 1. first\n- 2. second",
        "Acceptance checks\n## 1. first\n## 2. second",
        "Acceptance checks\n- explanatory prose only",
    ] {
        let invalid_or_empty = Issue {
            description: Some(description.into()),
            ..issue.clone()
        };
        let error = RuntimeService::delivery_note(
            &qa_task,
            &invalid_or_empty,
            &outcome("QA PASS: false vacuous pass\ntests run: cargo test\nchecks: arbitrary text"),
        )
        .expect_err("invalid or empty source criteria must fail before QA can pass");
        assert!(
            error.contains("acceptance"),
            "expected source-criteria error in {error}"
        );
    }

    // IDs are canonical decimal integers in the documented 1..=999 range.
    // The high boundary is accepted; every near miss is rejected for QA as
    // well as implementation/repair evidence.
    let bounded_issue = Issue {
        description: Some("Acceptance checks\n1. first\n999. last".into()),
        ..issue.clone()
    };
    assert!(
        RuntimeService::delivery_note(
            &qa_task,
            &bounded_issue,
            &outcome("QA PASS: boundary coverage\ntests run: cargo test\nchecks: 1, 999"),
        )
        .is_ok()
    );
    for invalid_checks in [
        "+1, 2",
        "-1, 2",
        "0, 2",
        "01, 2",
        "1, 1000",
        "1, 999999999999999999999999999999999999999",
        "1, 2, 2",
        "1, \u{00a0}2",
        "1, 2\u{200b}",
    ] {
        let error = RuntimeService::delivery_note(
            &qa_task,
            &issue,
            &outcome(&format!(
                "QA PASS: malformed check coverage\ntests run: cargo test\nchecks: {invalid_checks}"
            )),
        )
        .expect_err("noncanonical check references must fail closed");
        assert!(error.contains("checks"), "expected checks error in {error}");
    }

    let implementation_task = task(polyphony_core::PipelineTaskRole::Implementation);
    let implementation_note = "IMPLEMENTATION NOTE: evidence\nwhat changed: added a guard\ncommit: abc123\ntests run: cargo test\nchecks: 1, 2";
    for invisible in [
        "\u{00a0}", "\u{0600}", "\u{0301}", "\u{20dd}", "\u{200b}", "\u{202e}", "\u{2060}",
        "\u{0007}",
    ] {
        let error = RuntimeService::delivery_note(
            &implementation_task,
            &issue,
            &outcome(&implementation_note.replace("added a guard", invisible)),
        )
        .expect_err("Unicode whitespace/control-only evidence must be rejected");
        assert!(
            error.contains("what changed") && error.contains("visible"),
            "expected visible-value error in {error}"
        );
    }

    // Unicode prose (and harmless surrounding Unicode whitespace) is not
    // mistaken for an invisible value.
    assert!(
        RuntimeService::delivery_note(
            &implementation_task,
            &issue,
            &outcome(&implementation_note.replace(
                "added a guard",
                "\u{0600}\u{00a0}解析の修正\u{202e}\u{00a0}",
            ),),
        )
        .is_ok()
    );
}

#[tokio::test]
async fn qa_success_without_a_durable_verdict_cannot_mark_a_pipeline_passed() {
    let workspace_root = unique_workspace_root("qa-verdict-required");
    let workflow = test_workflow_with_front_matter(
        &workspace_root,
        "---\ntracker:\n  kind: mock\npolling:\n  interval_ms: 1000\nworkspace:\n  root: __ROOT__\norchestration:\n  dispatch_mode: manual\nagents:\n  default: qa\n  profiles:\n    qa: { kind: mock, transport: mock, command: mock }\npipeline:\n  stages:\n    - { category: review, role: qa, agent: qa }\n---\nQA fixture\n",
    );
    let (_tx, rx) = watch::channel(workflow.clone());
    let issue = sample_issue("issue-qa-verdict", "QA-18", "Todo", "Verdict required");
    let mut service = RuntimeService::new(
        Arc::new(TestTracker::new(vec![issue.clone()])),
        None,
        Arc::new(NoopAgent),
        Arc::new(RecordingProvisioner::default()),
        None,
        None,
        None,
        None,
        None,
        None,
        rx,
    )
    .0;
    service
        .dispatch_pipeline(workflow.clone(), issue.clone(), None, false, false, None)
        .await
        .unwrap();
    let run_id = service.state.runs.keys().next().unwrap().clone();
    let task_id = service.state.tasks[&run_id][0].id.clone();
    service.state.running.remove(&issue.id);
    service
        .handle_task_finished(
            &workflow,
            &issue,
            &run_id,
            &task_id,
            &workspace_root,
            &AgentRunResult::succeeded(1),
            None,
        )
        .await
        .unwrap();
    assert_eq!(service.state.tasks[&run_id][0].status, TaskStatus::Failed);
    assert_eq!(service.state.runs[&run_id].status, RunStatus::Failed);
    assert!(
        service.state.tasks[&run_id][0]
            .error
            .as_deref()
            .unwrap()
            .contains("durable QA PASS or QA FAIL")
    );
}

#[tokio::test]
async fn stop_mode_blocks_retries() {
    let workspace_root = unique_workspace_root("stop-retry");
    let tracker = TestTracker::new(vec![sample_issue("issue-1", "FAC-1", "Todo", "First")]);
    let provisioner = RecordingProvisioner::default();
    let mut service = test_service(tracker, provisioner, &workspace_root);
    service.state.dispatch_mode = polyphony_core::DispatchMode::Stop;
    // Manually insert a due retry.
    service.state.retrying.insert("issue-1".into(), RetryEntry {
        row: RetryRow {
            repo_id: String::new(),
            issue_id: "issue-1".into(),
            issue_identifier: "FAC-1".into(),
            attempt: 1,
            due_at: Utc::now() - chrono::Duration::seconds(10),
            error: Some("test error".into()),
        },
        due_at: Instant::now() - Duration::from_secs(10),
    });

    service.process_due_retries().await;

    assert!(
        service.state.retrying.contains_key("issue-1"),
        "retry should remain queued and not be processed in stop mode"
    );
    assert!(
        !service.state.running.contains_key("issue-1"),
        "no task should be dispatched from retry in stop mode"
    );
}

#[tokio::test]
async fn abort_all_drains_retry_queue() {
    let workspace_root = unique_workspace_root("stop-abort-retries");
    let tracker = TestTracker::new(Vec::new());
    let provisioner = RecordingProvisioner::default();
    let mut service = test_service(tracker, provisioner, &workspace_root);
    service.claim_issue("issue-1".to_string(), IssueClaimState::RetryQueued);
    service.state.retrying.insert("issue-1".into(), RetryEntry {
        row: RetryRow {
            repo_id: String::new(),
            issue_id: "issue-1".into(),
            issue_identifier: "FAC-1".into(),
            attempt: 2,
            due_at: Utc::now() + chrono::Duration::minutes(5),
            error: Some("transient".into()),
        },
        due_at: Instant::now() + Duration::from_secs(300),
    });

    service.abort_all().await;

    assert!(
        service.state.retrying.is_empty(),
        "abort_all should drain the retry queue"
    );
    assert!(
        !service.is_claimed("issue-1"),
        "abort_all should release claims for drained retries"
    );
}

#[tokio::test]
async fn finish_running_in_stop_mode_does_not_schedule_retry_on_success() {
    let workspace_root = unique_workspace_root("stop-finish-success");
    let provisioner = RecordingProvisioner::default();
    let mut service = test_service(TestTracker::new(Vec::new()), provisioner, &workspace_root);
    let issue = sample_issue("issue-5", "FAC-5", "Todo", "Work");
    let workspace_path = workspace_root.join("FAC-5");
    service.state.running.insert(
        issue.id.clone(),
        make_running_task(issue.clone(), workspace_path),
    );
    service.claim_issue(issue.id.clone(), IssueClaimState::Running);
    service.state.dispatch_mode = polyphony_core::DispatchMode::Stop;

    service
        .finish_running(
            issue.id.clone(),
            issue.identifier.clone(),
            None,
            Utc::now(),
            AgentRunResult {
                status: AttemptStatus::Succeeded,
                turns_completed: 1,
                error: None,
                final_issue_state: Some("Human Review".into()),
            },
        )
        .await
        .unwrap();

    assert!(
        service.state.completed.contains(&issue.id),
        "issue should still be marked as completed"
    );
    assert!(
        !service.state.retrying.contains_key(&issue.id),
        "no retry should be scheduled in stop mode"
    );
    assert!(
        !service.is_claimed(&issue.id),
        "issue claim should be released in stop mode"
    );
}

#[tokio::test]
async fn finish_running_in_stop_mode_does_not_schedule_retry_on_failure() {
    let workspace_root = unique_workspace_root("stop-finish-fail");
    let provisioner = RecordingProvisioner::default();
    let mut service = test_service(TestTracker::new(Vec::new()), provisioner, &workspace_root);
    let issue = sample_issue("issue-6", "FAC-6", "Todo", "Work");
    let workspace_path = workspace_root.join("FAC-6");
    service.state.running.insert(
        issue.id.clone(),
        make_running_task(issue.clone(), workspace_path),
    );
    service.claim_issue(issue.id.clone(), IssueClaimState::Running);
    service.state.dispatch_mode = polyphony_core::DispatchMode::Stop;

    service
        .finish_running(
            issue.id.clone(),
            issue.identifier.clone(),
            Some(1),
            Utc::now(),
            AgentRunResult {
                status: AttemptStatus::Failed,
                turns_completed: 0,
                error: Some("test failure".into()),
                final_issue_state: None,
            },
        )
        .await
        .unwrap();

    assert!(
        !service.state.retrying.contains_key(&issue.id),
        "no retry should be scheduled in stop mode after failure"
    );
    assert!(
        !service.is_claimed(&issue.id),
        "issue claim should be released in stop mode after failure"
    );
}

// ---------------------------------------------------------------------------
// Run deduplication tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn dispatch_reuses_active_run_for_same_issue() {
    let workspace_root = unique_workspace_root("run-reuse");
    let tracker = TestTracker::new(vec![sample_issue("issue-1", "FAC-1", "Todo", "First")]);
    let provisioner = RecordingProvisioner::default();
    let mut service = test_service(tracker, provisioner, &workspace_root);
    service.state.dispatch_mode = polyphony_core::DispatchMode::Automatic;

    // First dispatch creates a run.
    service.tick().await;
    assert!(service.state.running.contains_key("issue-1"));
    let run_count_after_first = service.state.runs.len();
    assert_eq!(
        run_count_after_first, 1,
        "first dispatch should create one run"
    );

    // Simulate the task finishing with success so it gets a continuation retry.
    handle_next_worker_message(&mut service).await;

    // The issue should now be in the retry queue with a run still present.
    assert!(
        service.state.retrying.contains_key("issue-1"),
        "successful finish should schedule a continuation retry"
    );

    // Process the retry (it fires after 1 second but we can trigger manually).
    service.state.retrying.get_mut("issue-1").unwrap().due_at =
        Instant::now() - Duration::from_secs(1);
    service.process_due_retries().await;

    // After the retry dispatch, there should still be only one run.
    let run_count_after_retry = service
        .state
        .runs
        .values()
        .filter(|m| m.issue_id.as_deref() == Some("issue-1"))
        .count();
    assert_eq!(
        run_count_after_retry, 1,
        "retry dispatch should reuse the existing run, not create a duplicate"
    );
}

#[tokio::test]
async fn dispatch_acknowledges_issue_on_first_attempt() {
    let workspace_root = unique_workspace_root("ack-dispatch");
    let tracker = TestTracker::new(vec![sample_issue("issue-1", "FAC-1", "Todo", "First")]);
    let provisioner = RecordingProvisioner::default();
    let mut service = test_service(tracker.clone(), provisioner, &workspace_root);
    service.state.dispatch_mode = polyphony_core::DispatchMode::Automatic;

    service.tick().await;
    assert!(service.state.running.contains_key("issue-1"));

    let acked = tracker.acknowledged_issues();
    assert_eq!(
        acked,
        vec!["issue-1"],
        "issue should be acknowledged on first dispatch"
    );
}

#[tokio::test]
async fn dispatch_does_not_acknowledge_on_retry() {
    let workspace_root = unique_workspace_root("ack-retry");
    let tracker = TestTracker::new(vec![sample_issue("issue-1", "FAC-1", "Todo", "First")]);
    let provisioner = RecordingProvisioner::default();
    let mut service = test_service(tracker.clone(), provisioner, &workspace_root);
    service.state.dispatch_mode = polyphony_core::DispatchMode::Automatic;

    // First dispatch — should acknowledge.
    service.tick().await;
    assert_eq!(tracker.acknowledged_issues().len(), 1);

    // Simulate worker finishing with success so it queues a retry.
    handle_next_worker_message(&mut service).await;
    assert!(service.state.retrying.contains_key("issue-1"));

    // Trigger retry — should NOT acknowledge again.
    service.state.retrying.get_mut("issue-1").unwrap().due_at =
        Instant::now() - Duration::from_secs(1);
    service.process_due_retries().await;

    assert_eq!(
        tracker.acknowledged_issues().len(),
        1,
        "retry dispatch must not re-acknowledge the issue"
    );
}

#[test]
fn find_existing_run_prefers_active_over_terminal() {
    let workspace_root = unique_workspace_root("run-find-existing");
    let tracker = TestTracker::new(Vec::new());
    let provisioner = RecordingProvisioner::default();
    let mut service = test_service(tracker, provisioner, &workspace_root);

    let now = Utc::now();

    // Insert a delivered (terminal) run for the issue.
    service.state.runs.insert("run-delivered".into(), Run {
        id: "run-delivered".into(),
        kind: RunKind::IssueDelivery,
        issue_id: Some("issue-1".into()),
        issue_identifier: Some("FAC-1".into()),
        title: "Delivered work".into(),
        status: RunStatus::Delivered,
        pipeline_stage: None,
        manual_dispatch_directives: None,
        workspace_key: None,
        workspace_path: None,
        review_target: None,
        deliverable: None,
        created_at: now,
        activity_log: Vec::new(),
        cancel_reason: None,
        blocked_outcome: None,
        steps: Vec::new(),
        updated_at: now,
    });

    // Even a terminal run should be found — prevents duplicate runs
    // when an issue is re-dispatched via continuation retry.
    assert_eq!(
        service.find_existing_run_for_issue("issue-1"),
        Some("run-delivered".into()),
        "delivered run should be found when no active one exists"
    );

    // Insert an in-progress (active) run — should be preferred.
    service.state.runs.insert("run-active".into(), Run {
        id: "run-active".into(),
        kind: RunKind::IssueDelivery,
        issue_id: Some("issue-1".into()),
        issue_identifier: Some("FAC-1".into()),
        title: "Active work".into(),
        status: RunStatus::InProgress,
        pipeline_stage: None,
        manual_dispatch_directives: None,
        workspace_key: None,
        workspace_path: None,
        review_target: None,
        deliverable: None,
        created_at: now,
        activity_log: Vec::new(),
        cancel_reason: None,
        blocked_outcome: None,
        steps: Vec::new(),
        updated_at: now,
    });

    assert_eq!(
        service.find_existing_run_for_issue("issue-1"),
        Some("run-active".into()),
        "active run should be preferred over terminal one"
    );

    // No run for a different issue.
    assert!(
        service.find_existing_run_for_issue("issue-999").is_none(),
        "should return None for an issue with no runs"
    );
}

// ---------------------------------------------------------------------------
// Dispatch mode persistence tests
// ---------------------------------------------------------------------------

#[test]
fn restore_bootstrap_preserves_persisted_dispatch_mode() {
    let workspace_root = unique_workspace_root("mode-persist");
    let tracker = TestTracker::new(Vec::new());
    let provisioner = RecordingProvisioner::default();
    let mut service = test_service(tracker, provisioner, &workspace_root);

    // Default before bootstrap is Manual (from test config).
    assert_eq!(service.state.dispatch_mode, DispatchMode::Manual);
    assert!(!service.state.bootstrap_restored);

    let now = Utc::now();
    service.restore_bootstrap(StoreBootstrap {
        snapshot: Some(RuntimeSnapshot {
            repo_ids: Vec::new(),
            repo_registrations: Vec::new(),
            generated_at: now,
            counts: SnapshotCounts::default(),
            cadence: RuntimeCadence::default(),
            tracker_issues: Vec::new(),
            inbox_items: Vec::new(),
            approved_inbox_keys: Vec::new(),
            running: Vec::new(),
            agent_run_history: Vec::new(),
            retrying: Vec::new(),
            codex_totals: CodexTotals::default(),
            rate_limits: None,
            throttles: Vec::new(),
            budgets: Vec::new(),
            agent_catalogs: Vec::new(),
            saved_contexts: Vec::new(),
            recent_events: Vec::new(),
            pending_user_interactions: Vec::new(),
            runs: Vec::new(),
            tasks: Vec::new(),
            loading: LoadingState::default(),
            dispatch_mode: DispatchMode::Stop,
            tracker_kind: TrackerKind::default(),
            tracker_connection: None,
            from_cache: false,
            cached_at: None,
            agent_profile_names: Vec::new(),
            agent_profiles: Vec::new(),
            heartbeat: polyphony_core::HeartbeatStatus::default(),
        }),
        retrying: std::collections::HashMap::new(),
        throttles: std::collections::HashMap::new(),
        budgets: std::collections::HashMap::new(),
        saved_contexts: std::collections::HashMap::new(),
        recent_events: Vec::new(),
        runs: std::collections::HashMap::new(),
        tasks: std::collections::HashMap::new(),
        reviewed_pull_request_heads: std::collections::HashMap::new(),
        agent_run_history: Vec::new(),
    });

    assert!(service.state.bootstrap_restored);
    assert_eq!(
        service.state.dispatch_mode,
        DispatchMode::Stop,
        "dispatch mode should be restored from snapshot"
    );
}

#[tokio::test]
async fn normalize_restored_cancelled_run_keeps_task_terminal() {
    let workspace_root = unique_workspace_root("normalize-stale-running-task");
    let issue = sample_issue(
        "issue-restored-cancelled",
        "DOG-811",
        "Todo",
        "Cancelled pipeline",
    );
    let workflow = pipeline_workflow_with_automation(&workspace_root);
    let tracker = TestTracker::new(vec![issue.clone()]);
    let tracker_handle = tracker.clone();
    let provisioner = RecordingProvisioner::default();
    let mut service = test_service_for_workflow(workflow, tracker, provisioner);
    let now = Utc::now();
    let run_id = "run-stale-running".to_string();
    let task_id = "task-stale-running".to_string();

    service.restore_bootstrap(StoreBootstrap {
        snapshot: None,
        retrying: std::collections::HashMap::new(),
        throttles: std::collections::HashMap::new(),
        budgets: std::collections::HashMap::new(),
        saved_contexts: std::collections::HashMap::new(),
        recent_events: Vec::new(),
        runs: std::collections::HashMap::from([(run_id.clone(), polyphony_core::Run {
            id: run_id.clone(),
            kind: polyphony_core::RunKind::IssueDelivery,
            issue_id: Some(issue.id.clone()),
            issue_identifier: Some(issue.identifier.clone()),
            title: issue.title.clone(),
            status: polyphony_core::RunStatus::Cancelled,
            pipeline_stage: Some(polyphony_core::PipelineStage::Executing),
            manual_dispatch_directives: None,
            workspace_key: Some(sanitize_workspace_key(&issue.identifier)),
            workspace_path: Some(workspace_root.join(sanitize_workspace_key(&issue.identifier))),
            review_target: None,
            deliverable: None,
            created_at: now,
            activity_log: Vec::new(),
            cancel_reason: Some("eligibility revoked".into()),
            blocked_outcome: None,
            steps: Vec::new(),
            updated_at: now,
        })]),
        tasks: std::collections::HashMap::from([(task_id.clone(), polyphony_core::Task {
            id: task_id.clone(),
            run_id: run_id.clone(),
            title: "Interrupted pipeline task".into(),
            description: None,
            activity_log: Vec::new(),
            category: polyphony_core::TaskCategory::Review,
            role: polyphony_core::PipelineTaskRole::Implementation,
            status: polyphony_core::TaskStatus::InProgress,
            ordinal: 1,
            parent_id: None,
            agent_name: Some("reviewer".into()),
            session_id: None,
            thread_id: None,
            turns_completed: 0,
            tokens: TokenUsage::default(),
            started_at: Some(now),
            finished_at: None,
            error: None,
            created_at: now,
            updated_at: now,
        })]),
        reviewed_pull_request_heads: std::collections::HashMap::new(),
        agent_run_history: Vec::new(),
    });

    service.normalize_restored_in_progress_runs().await.unwrap();
    // Exercise a real post-restart poll tick after bootstrap normalization.
    // Manual dispatch mode keeps the fixture deterministic while proving that
    // the restored pipeline run does not enter planner or worker dispatch.
    service.tick().await;

    let run = service.state.runs.get(&run_id).unwrap();
    assert_eq!(run.status, polyphony_core::RunStatus::Cancelled);
    let task = service.state.tasks.get(&run_id).unwrap().first().unwrap();
    assert_eq!(task.status, polyphony_core::TaskStatus::Cancelled);
    assert_eq!(task.error.as_deref(), Some("eligibility revoked"));
    assert!(task.finished_at.is_some());
    assert!(service.state.running.is_empty());
    assert!(service.state.retrying.is_empty());
    assert!(service.pending_manual_dispatches.is_empty());
    assert!(service.pending_webhook_dispatches.is_empty());
    assert!(service.pending_task_retries.is_empty());
    assert!(service.pending_run_retries.is_empty());
    assert!(service.pending_feedback_injections.is_empty());
    assert!(tracker_handle.acknowledged_issues().is_empty());
    assert!(
        !service.state.recent_events.iter().any(|event| {
            event.message.contains("pipeline dispatched")
                || event.message.contains("re-running planner")
        }),
        "a restored cancelled pipeline must not plan or dispatch on restart"
    );
}

#[tokio::test]
async fn normalize_restored_in_progress_runs_marks_first_pending_task_failed() {
    let workspace_root = unique_workspace_root("normalize-stale-pending-task");
    let tracker = TestTracker::new(Vec::new());
    let provisioner = RecordingProvisioner::default();
    let mut service = test_service(tracker, provisioner, &workspace_root);
    let now = Utc::now();
    let run_id = "run-stale-pending".to_string();
    let workspace_task_id = "task-worktree".to_string();
    let review_task_id = "task-review".to_string();

    service.restore_bootstrap(StoreBootstrap {
        snapshot: None,
        retrying: std::collections::HashMap::new(),
        throttles: std::collections::HashMap::new(),
        budgets: std::collections::HashMap::new(),
        saved_contexts: std::collections::HashMap::new(),
        recent_events: Vec::new(),
        runs: std::collections::HashMap::from([(run_id.clone(), polyphony_core::Run {
            id: run_id.clone(),
            kind: polyphony_core::RunKind::PullRequestReview,
            issue_id: Some("issue-89".into()),
            issue_identifier: Some("penso/arbor#89".into()),
            title: "Review PR".into(),
            status: polyphony_core::RunStatus::InProgress,
            pipeline_stage: None,
            manual_dispatch_directives: None,
            workspace_key: Some("penso_arbor_89".into()),
            workspace_path: Some(workspace_root.join("penso_arbor_89")),
            review_target: None,
            deliverable: None,
            created_at: now,
            activity_log: Vec::new(),
            cancel_reason: None,
            blocked_outcome: None,
            steps: Vec::new(),
            updated_at: now,
        })]),
        tasks: std::collections::HashMap::from([
            (workspace_task_id.clone(), polyphony_core::Task {
                id: workspace_task_id.clone(),
                run_id: run_id.clone(),
                title: "Creating worktree".into(),
                description: None,
                activity_log: Vec::new(),
                category: polyphony_core::TaskCategory::Research,
                role: polyphony_core::PipelineTaskRole::Implementation,
                status: polyphony_core::TaskStatus::Completed,
                ordinal: 0,
                parent_id: None,
                agent_name: Some("orchestrator".into()),
                session_id: None,
                thread_id: None,
                turns_completed: 0,
                tokens: TokenUsage::default(),
                started_at: Some(now),
                finished_at: Some(now),
                error: None,
                created_at: now,
                updated_at: now,
            }),
            (review_task_id.clone(), polyphony_core::Task {
                id: review_task_id.clone(),
                run_id: run_id.clone(),
                title: "Run PR review".into(),
                description: None,
                activity_log: Vec::new(),
                category: polyphony_core::TaskCategory::Review,
                role: polyphony_core::PipelineTaskRole::Implementation,
                status: polyphony_core::TaskStatus::Pending,
                ordinal: 1,
                parent_id: None,
                agent_name: Some("reviewer".into()),
                session_id: None,
                thread_id: None,
                turns_completed: 0,
                tokens: TokenUsage::default(),
                started_at: None,
                finished_at: None,
                error: None,
                created_at: now,
                updated_at: now,
            }),
        ]),
        reviewed_pull_request_heads: std::collections::HashMap::new(),
        agent_run_history: Vec::new(),
    });

    service.normalize_restored_in_progress_runs().await.unwrap();

    let run = service.state.runs.get(&run_id).unwrap();
    assert_eq!(run.status, polyphony_core::RunStatus::Failed);
    let tasks = service.state.tasks.get(&run_id).unwrap();
    let workspace_task = tasks
        .iter()
        .find(|task| task.id == workspace_task_id)
        .unwrap();
    assert_eq!(workspace_task.status, polyphony_core::TaskStatus::Completed);
    let review_task = tasks.iter().find(|task| task.id == review_task_id).unwrap();
    assert_eq!(review_task.status, polyphony_core::TaskStatus::Failed);
    assert_eq!(
        review_task.error.as_deref(),
        Some("restored without an active agent session; retry the run to continue")
    );
}

#[tokio::test]
async fn tick_populates_repo_snapshot_metadata_for_registered_repos() {
    let workspace_root = std::env::temp_dir().join(format!(
        "polyphony-multi-repo-snapshot-{}",
        uuid::Uuid::new_v4()
    ));
    let primary_root = workspace_root.join("primary");
    let secondary_root = workspace_root.join("secondary");
    let primary_workflow = test_workflow(&primary_root);
    let secondary_workflow = test_workflow(&secondary_root);
    let issue = sample_issue("repo-issue-1", "REP-1", "Todo", "Secondary issue");
    let registration = RepoRegistration {
        repo_id: "owner/repo".into(),
        label: "owner/repo".into(),
        worktree_path: secondary_root.clone(),
        clone_url: None,
        default_branch: "main".into(),
        tracker_kind: secondary_workflow.config.tracker.kind,
        added_at: Utc::now(),
    };
    let secondary_components = RuntimeComponents {
        tracker: Arc::new(TestTracker::new(vec![issue.clone()])),
        pull_request_event_source: None,
        agent: Arc::new(NoopAgent),
        committer: None,
        pull_request_manager: None,
        pull_request_commenter: None,
        feedback: None,
    };
    let mut repos = std::collections::HashMap::new();
    repos.insert(
        registration.repo_id.clone(),
        RepoContext::from_components(
            registration.clone(),
            secondary_workflow,
            &secondary_components,
        ),
    );
    let (_tx, rx) = watch::channel(primary_workflow);
    let provisioner = RecordingProvisioner::default();
    let mut service = RuntimeService::new_with_repos(
        Arc::new(TestTracker::new(Vec::new())),
        None,
        Arc::new(NoopAgent),
        Arc::new(provisioner),
        None,
        None,
        None,
        None,
        None,
        None,
        rx,
        repos,
    )
    .0;

    assert!(!service.tick().await);
    let snapshot = service.snapshot();
    assert_eq!(snapshot.repo_ids, vec![registration.repo_id.clone()]);
    assert_eq!(snapshot.repo_registrations.len(), 1);
    assert_eq!(snapshot.repo_registrations[0].repo_id, registration.repo_id);
    assert_eq!(snapshot.tracker_issues.len(), 1);
    assert_eq!(snapshot.tracker_issues[0].repo_id, "owner/repo");
    assert_eq!(snapshot.inbox_items.len(), 1);
    assert_eq!(snapshot.inbox_items[0].repo_id, "owner/repo");
}

#[test]
fn handle_add_repo_builds_context_via_factory() {
    let workspace_root =
        std::env::temp_dir().join(format!("polyphony-multi-repo-add-{}", uuid::Uuid::new_v4()));
    let primary_root = workspace_root.join("primary");
    let added_root = workspace_root.join("added");
    let workflow = test_workflow(&primary_root);
    let (_tx, rx) = watch::channel(workflow);
    let provisioner = RecordingProvisioner::default();
    let repo_factory: Arc<RepoContextFactory> = Arc::new(|registration| {
        let workflow = test_workflow(&registration.worktree_path);
        let components = RuntimeComponents {
            tracker: Arc::new(TestTracker::new(Vec::new())),
            pull_request_event_source: None,
            agent: Arc::new(NoopAgent),
            committer: None,
            pull_request_manager: None,
            pull_request_commenter: None,
            feedback: None,
        };
        Ok(RepoContext::from_components(
            registration.clone(),
            workflow,
            &components,
        ))
    });
    let mut service = RuntimeService::new(
        Arc::new(TestTracker::new(Vec::new())),
        None,
        Arc::new(NoopAgent),
        Arc::new(provisioner),
        None,
        None,
        None,
        None,
        None,
        None,
        rx,
    )
    .0
    .with_repo_context_factory(repo_factory);
    let registration = RepoRegistration {
        repo_id: "owner/added".into(),
        label: "owner/added".into(),
        worktree_path: added_root,
        clone_url: None,
        default_branch: "main".into(),
        tracker_kind: TrackerKind::Mock,
        added_at: Utc::now(),
    };

    service.handle_add_repo(registration.clone());

    assert!(service.repos.contains_key(&registration.repo_id));
    assert!(service.pending_refresh);
    assert_eq!(
        service.repos[&registration.repo_id].registration.repo_id,
        registration.repo_id
    );
}

#[tokio::test]
async fn process_pending_create_issues_routes_to_requested_repo() {
    let workspace_root = unique_workspace_root("create-issue-routed");
    let primary_root = workspace_root.join("primary");
    let secondary_root = workspace_root.join("secondary");
    let primary_workflow = test_workflow(&primary_root);
    let secondary_workflow = test_workflow(&secondary_root);
    let primary_tracker = Arc::new(TestTracker::new(Vec::new()));
    let secondary_tracker = Arc::new(TestTracker::new(Vec::new()));
    let secondary_registration = RepoRegistration {
        repo_id: "owner/secondary".into(),
        label: "owner/secondary".into(),
        worktree_path: secondary_root,
        clone_url: None,
        default_branch: "main".into(),
        tracker_kind: secondary_workflow.config.tracker.kind,
        added_at: Utc::now(),
    };
    let mut repos = HashMap::new();
    repos.insert(
        secondary_registration.repo_id.clone(),
        RepoContext::from_components(
            secondary_registration.clone(),
            secondary_workflow,
            &RuntimeComponents {
                tracker: secondary_tracker.clone(),
                pull_request_event_source: None,
                agent: Arc::new(NoopAgent),
                committer: None,
                pull_request_manager: None,
                pull_request_commenter: None,
                feedback: None,
            },
        ),
    );
    let (_tx, rx) = watch::channel(primary_workflow);
    let provisioner = RecordingProvisioner::default();
    let mut service = RuntimeService::new_with_repos(
        primary_tracker.clone(),
        None,
        Arc::new(NoopAgent),
        Arc::new(provisioner),
        None,
        None,
        None,
        None,
        None,
        None,
        rx,
        repos,
    )
    .0;
    service
        .pending_create_issues
        .push(CreateIssueCommandRequest {
            title: "Routed issue".into(),
            description: "details".into(),
            repo_id: Some(secondary_registration.repo_id.clone()),
        });

    service.process_pending_create_issues().await;

    assert!(service.pending_refresh);
    assert!(primary_tracker.recorded_create_issues().is_empty());
    assert_eq!(secondary_tracker.recorded_create_issues().len(), 1);
    assert_eq!(
        secondary_tracker.recorded_create_issues()[0].title,
        "Routed issue"
    );
    assert_eq!(
        service
            .state
            .issue_repo_map
            .get("created-1")
            .map(String::as_str),
        Some("owner/secondary")
    );
}

#[tokio::test]
async fn process_pending_create_issues_rejects_ambiguous_repo_without_repo_id() {
    let workspace_root = unique_workspace_root("create-issue-ambiguous");
    let primary_root = workspace_root.join("primary");
    let secondary_root = workspace_root.join("secondary");
    let primary_workflow = test_workflow(&primary_root);
    let secondary_workflow = test_workflow(&secondary_root);
    let primary_tracker = Arc::new(TestTracker::new(Vec::new()));
    let secondary_tracker = Arc::new(TestTracker::new(Vec::new()));
    let secondary_registration = RepoRegistration {
        repo_id: "owner/secondary".into(),
        label: "owner/secondary".into(),
        worktree_path: secondary_root,
        clone_url: None,
        default_branch: "main".into(),
        tracker_kind: secondary_workflow.config.tracker.kind,
        added_at: Utc::now(),
    };
    let mut repos = HashMap::new();
    repos.insert(
        secondary_registration.repo_id.clone(),
        RepoContext::from_components(
            secondary_registration,
            secondary_workflow,
            &RuntimeComponents {
                tracker: secondary_tracker.clone(),
                pull_request_event_source: None,
                agent: Arc::new(NoopAgent),
                committer: None,
                pull_request_manager: None,
                pull_request_commenter: None,
                feedback: None,
            },
        ),
    );
    repos.insert(
        "owner/primary".into(),
        RepoContext::from_components(
            RepoRegistration {
                repo_id: "owner/primary".into(),
                label: "owner/primary".into(),
                worktree_path: primary_root.clone(),
                clone_url: None,
                default_branch: "main".into(),
                tracker_kind: primary_workflow.config.tracker.kind,
                added_at: Utc::now(),
            },
            primary_workflow.clone(),
            &RuntimeComponents {
                tracker: primary_tracker.clone(),
                pull_request_event_source: None,
                agent: Arc::new(NoopAgent),
                committer: None,
                pull_request_manager: None,
                pull_request_commenter: None,
                feedback: None,
            },
        ),
    );
    let (_tx, rx) = watch::channel(primary_workflow);
    let provisioner = RecordingProvisioner::default();
    let mut service = RuntimeService::new_with_repos(
        primary_tracker.clone(),
        None,
        Arc::new(NoopAgent),
        Arc::new(provisioner),
        None,
        None,
        None,
        None,
        None,
        None,
        rx,
        repos,
    )
    .0;
    service
        .pending_create_issues
        .push(CreateIssueCommandRequest {
            title: "Ambiguous issue".into(),
            description: "details".into(),
            repo_id: None,
        });

    service.process_pending_create_issues().await;

    assert!(!service.pending_refresh);
    assert!(primary_tracker.recorded_create_issues().is_empty());
    assert!(secondary_tracker.recorded_create_issues().is_empty());
    assert!(
        service.state.recent_events.iter().any(|event| event
            .message
            .contains("repo_id is required when multiple repositories are registered")),
        "runtime should explain why issue creation was rejected"
    );
}
