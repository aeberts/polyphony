use crate::{prelude::*, *};

impl RuntimeService {
    pub(crate) fn configured_blocked_state(
        &self,
        workflow: &LoadedWorkflow,
    ) -> Result<String, Error> {
        let state = workflow
            .config
            .tracker
            .blocked_state
            .as_deref()
            .map(str::trim)
            .filter(|state| !state.is_empty())
            .ok_or_else(|| {
                Error::Core(CoreError::Adapter(
                    "blocked outcome rejected: tracker.blocked_state is required".into(),
                ))
            })?;
        if !workflow
            .config
            .tracker
            .active_states
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(state))
        {
            return Err(Error::Core(CoreError::Adapter(format!(
                "blocked outcome rejected: tracker.blocked_state `{state}` is not in tracker.active_states"
            ))));
        }
        Ok(state.to_string())
    }

    pub(crate) fn issue_is_in_blocked_state(
        &self,
        workflow: &LoadedWorkflow,
        issue_state: &str,
    ) -> bool {
        self.configured_blocked_state(workflow)
            .is_ok_and(|blocked_state| blocked_state.eq_ignore_ascii_case(issue_state))
    }

    pub(crate) async fn record_blocked_outcome(
        &mut self,
        workflow: &LoadedWorkflow,
        issue: &Issue,
        run_id: Option<&str>,
        task_id: Option<&str>,
        outcome: &polyphony_core::BlockedOutcome,
    ) -> Result<(), Error> {
        let blocked_state = self.configured_blocked_state(workflow)?;
        let run_id = run_id
            .map(ToOwned::to_owned)
            .or_else(|| self.find_existing_run_for_issue(&issue.id))
            .ok_or_else(|| {
                Error::Core(CoreError::Adapter(
                    "blocked outcome rejected: no durable run exists for the issue".into(),
                ))
            })?;
        let tracker = self.tracker_for_issue(&issue.id);
        let comment = format!(
            "## Polyphony blocked outcome\n\nReason: {}\n\nEvidence: {}\n\nPrerequisite work: {}\n\nConfigured workflow state: `{blocked_state}`",
            outcome.reason, outcome.evidence, outcome.prerequisite
        );

        // Tracker writes precede the terminal local commit. If either tracker
        // write fails, this method returns without setting local blocked state.
        tracker
            .comment_on_issue(&polyphony_core::AddIssueCommentRequest {
                id: issue.id.clone(),
                body: comment,
            })
            .await?;
        tracker
            .update_issue_workflow_status(issue, &blocked_state)
            .await?;

        let now = Utc::now();
        let run = self.state.runs.get_mut(&run_id).ok_or_else(|| {
            Error::Core(CoreError::Adapter(format!(
                "blocked outcome rejected: run {run_id} disappeared before terminal commit"
            )))
        })?;
        run.status = RunStatus::Blocked;
        run.blocked_outcome = Some(outcome.clone());
        run.updated_at = now;
        run.push_log(
            polyphony_core::RunLogScope::Pipeline,
            format!(
                "blocked pending {}: {}",
                outcome.prerequisite, outcome.reason
            ),
        );
        if let Some(store) = &self.store {
            store.save_run(run).await?;
        }

        if let Some(task_id) = task_id
            && let Some(tasks) = self.state.tasks.get_mut(&run_id)
            && let Some(task) = tasks.iter_mut().find(|task| task.id == task_id)
        {
            task.status = TaskStatus::Cancelled;
            task.error = Some(format!("blocked: {}", outcome.reason));
            task.finished_at = Some(now);
            task.updated_at = now;
            if let Some(store) = &self.store {
                store.save_task(task).await?;
            }
        }

        self.state.retrying.remove(&issue.id);
        self.release_issue(&issue.id);
        self.push_event(
            EventScope::Worker,
            format!(
                "{} blocked pending {}",
                issue.identifier, outcome.prerequisite
            ),
        );
        Ok(())
    }
}
