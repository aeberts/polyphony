use crate::{prelude::*, *};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct AgentModel {
    pub id: String,
    pub display_name: Option<String>,
    pub created_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AgentModelCatalog {
    pub agent_name: String,
    pub provider_kind: String,
    pub fetched_at: DateTime<Utc>,
    pub selected_model: Option<String>,
    pub models: Vec<AgentModel>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum AgentEventKind {
    SessionStarted,
    TurnStarted,
    TurnCompleted,
    TurnFailed,
    TurnCancelled,
    ToolCallStarted,
    ToolCallCompleted,
    ToolCallFailed,
    Notification,
    UsageUpdated,
    RateLimitsUpdated,
    StartupFailed,
    OtherMessage,
    Outcome,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentEvent {
    pub issue_id: String,
    pub issue_identifier: String,
    pub agent_name: String,
    pub session_id: Option<String>,
    pub thread_id: Option<String>,
    pub turn_id: Option<String>,
    pub codex_app_server_pid: Option<String>,
    pub kind: AgentEventKind,
    pub at: DateTime<Utc>,
    pub message: Option<String>,
    pub usage: Option<TokenUsage>,
    pub rate_limits: Option<Value>,
    pub raw: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRunResult {
    pub status: AttemptStatus,
    pub turns_completed: u32,
    pub error: Option<String>,
    pub final_issue_state: Option<String>,
}

/// A worker-reported reason that its run cannot proceed until prerequisite work
/// is complete. The record is parsed from the strict `BLOCKED:` report format
/// before the orchestrator persists it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BlockedOutcome {
    pub reason: String,
    pub evidence: String,
    pub prerequisite: String,
}

impl AgentRunResult {
    pub fn succeeded(turns: u32) -> Self {
        Self {
            status: AttemptStatus::Succeeded,
            turns_completed: turns,
            error: None,
            final_issue_state: None,
        }
    }

    pub fn failed(error: impl Into<String>) -> Self {
        Self {
            status: AttemptStatus::Failed,
            turns_completed: 0,
            error: Some(error.into()),
            final_issue_state: None,
        }
    }

    pub fn cancelled(error: impl Into<String>) -> Self {
        Self {
            status: AttemptStatus::CancelledByReconciliation,
            turns_completed: 0,
            error: Some(error.into()),
            final_issue_state: None,
        }
    }

    /// Parse a structured blocked report, if this result reports one.
    ///
    /// The grammar is deliberately small and closed:
    ///
    /// ```text
    /// BLOCKED:
    /// reason: <non-empty>
    /// evidence: <non-empty>
    /// prerequisite: <non-empty linked work reference>
    /// ```
    ///
    /// This prevents prose that merely mentions a block from becoming a
    /// durable terminal state.
    pub fn blocked_outcome(&self) -> Result<Option<BlockedOutcome>, String> {
        let Some(report) = self.final_issue_state.as_deref() else {
            return Ok(None);
        };
        let mut lines = report.lines();
        let Some(header) = lines.next() else {
            return Ok(None);
        };
        if header.trim() != "BLOCKED:" {
            return Ok(None);
        }

        let mut reason = None;
        let mut evidence = None;
        let mut prerequisite = None;
        for line in lines {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let Some((key, value)) = line.split_once(':') else {
                return Err("blocked outcome fields must use `key: value`".into());
            };
            let value = value.trim();
            if value.is_empty() {
                return Err(format!("blocked outcome `{key}` must be non-empty"));
            }
            let field = match key.trim() {
                "reason" => &mut reason,
                "evidence" => &mut evidence,
                "prerequisite" => &mut prerequisite,
                _ => return Err(format!("blocked outcome has unknown field `{}`", key.trim())),
            };
            if field.replace(value.to_string()).is_some() {
                return Err(format!("blocked outcome repeats `{}`", key.trim()));
            }
        }

        let reason = reason.ok_or_else(|| "blocked outcome is missing `reason`".to_string())?;
        let evidence =
            evidence.ok_or_else(|| "blocked outcome is missing `evidence`".to_string())?;
        let prerequisite = prerequisite
            .ok_or_else(|| "blocked outcome is missing `prerequisite`".to_string())?;
        Ok(Some(BlockedOutcome {
            reason,
            evidence,
            prerequisite,
        }))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn parses_complete_blocked_outcome() {
        let result = AgentRunResult {
            status: AttemptStatus::Succeeded,
            turns_completed: 1,
            error: None,
            final_issue_state: Some(
                "BLOCKED:\nreason: API contract is absent\nevidence: integration test shows 404\nprerequisite: POL-42".into(),
            ),
        };

        assert_eq!(
            result.blocked_outcome().unwrap(),
            Some(BlockedOutcome {
                reason: "API contract is absent".into(),
                evidence: "integration test shows 404".into(),
                prerequisite: "POL-42".into(),
            })
        );
    }

    #[test]
    fn rejects_incomplete_or_duplicated_blocked_outcome() {
        for report in [
            "BLOCKED:\nreason: waiting\nevidence: trace",
            "BLOCKED:\nreason: waiting\nevidence: trace\nprerequisite: POL-42\nreason: again",
            "BLOCKED:\nreason: waiting\nevidence:\nprerequisite: POL-42",
        ] {
            let result = AgentRunResult {
                status: AttemptStatus::Succeeded,
                turns_completed: 1,
                error: None,
                final_issue_state: Some(report.into()),
            };
            assert!(result.blocked_outcome().is_err(), "{report}");
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum AgentProfileSource {
    #[default]
    Config,
    UserGlobal,
    Repository,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum AgentTransport {
    #[default]
    Mock,
    AppServer,
    Rpc,
    LocalCli,
    Acp,
    Acpx,
    OpenAiChat,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum AgentInteractionMode {
    #[default]
    OneShot,
    Interactive,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum AgentPromptMode {
    #[default]
    Env,
    Stdin,
    TmuxPaste,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum PtyBackendKind {
    #[default]
    PortablePty,
    PtyProcess,
}

impl PtyBackendKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PortablePty => "portable-pty",
            Self::PtyProcess => "pty-process",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AgentDefinition {
    pub name: String,
    pub kind: String,
    pub transport: AgentTransport,
    pub command: Option<String>,
    pub fallback_agents: Vec<String>,
    pub model: Option<String>,
    pub reasoning_level: Option<String>,
    pub models: Vec<String>,
    pub models_command: Option<String>,
    pub fetch_models: bool,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub approval_policy: Option<String>,
    pub thread_sandbox: Option<String>,
    pub turn_sandbox_policy: Option<String>,
    pub turn_timeout_ms: u64,
    pub read_timeout_ms: u64,
    pub stall_timeout_ms: i64,
    pub credits_command: Option<String>,
    pub spending_command: Option<String>,
    pub use_tmux: bool,
    pub tmux_session_prefix: Option<String>,
    pub interaction_mode: AgentInteractionMode,
    pub prompt_mode: AgentPromptMode,
    pub idle_timeout_ms: u64,
    pub completion_sentinel: Option<String>,
    pub env: BTreeMap<String, String>,
    pub pty_backend: PtyBackendKind,
}

#[derive(Debug, Clone)]
pub struct AgentRunSpec {
    pub issue: Issue,
    pub attempt: Option<u32>,
    pub workspace_path: PathBuf,
    pub prompt: String,
    pub max_turns: u32,
    pub agent: AgentDefinition,
    pub prior_context: Option<AgentContextSnapshot>,
}

#[async_trait]
pub trait AgentSession: Send {
    async fn run_turn(&mut self, prompt: String) -> Result<AgentRunResult, Error>;

    async fn stop(&mut self) -> Result<(), Error> {
        Ok(())
    }
}
