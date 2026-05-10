#[cfg(not(feature = "tui"))]
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};
use std::{future::Future, pin::Pin};

#[cfg(feature = "tui")]
pub(crate) use polyphony_tui::LogBuffer;

use crate::TuiVariant;

#[derive(Debug, thiserror::Error)]
pub(crate) enum TuiError {
    #[allow(dead_code)]
    #[error("tui variant `{0}` is not enabled for this build")]
    Disabled(&'static str),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

pub(crate) type TuiRunFuture = Pin<Box<dyn Future<Output = Result<(), TuiError>> + Send>>;

#[cfg(feature = "tui")]
pub(crate) const fn current_tui_available() -> bool {
    true
}

#[cfg(not(feature = "tui"))]
pub(crate) const fn current_tui_available() -> bool {
    false
}

#[cfg(feature = "tui-next")]
pub(crate) const fn next_tui_available() -> bool {
    true
}

#[cfg(not(feature = "tui-next"))]
pub(crate) const fn next_tui_available() -> bool {
    false
}

pub(crate) const fn tui_available(variant: TuiVariant) -> bool {
    match variant {
        TuiVariant::Current => current_tui_available(),
        TuiVariant::Next => next_tui_available(),
    }
}

pub(crate) async fn run(
    variant: TuiVariant,
    snapshot_rx: tokio::sync::watch::Receiver<polyphony_core::RuntimeSnapshot>,
    command_tx: tokio::sync::mpsc::UnboundedSender<polyphony_orchestrator::RuntimeCommand>,
    log_buffer: LogBuffer,
) -> Result<(), TuiError> {
    match variant {
        TuiVariant::Current => run_current(snapshot_rx, command_tx, log_buffer).await,
        TuiVariant::Next => run_next(snapshot_rx, command_tx).await,
    }
}

#[cfg(feature = "tui")]
async fn run_current(
    snapshot_rx: tokio::sync::watch::Receiver<polyphony_core::RuntimeSnapshot>,
    command_tx: tokio::sync::mpsc::UnboundedSender<polyphony_orchestrator::RuntimeCommand>,
    log_buffer: LogBuffer,
) -> Result<(), TuiError> {
    polyphony_tui::run(snapshot_rx, command_tx, log_buffer)
        .await
        .map_err(|error| match error {
            polyphony_tui::Error::Io(error) => TuiError::Io(error),
        })
}

#[cfg(not(feature = "tui"))]
async fn run_current(
    _snapshot_rx: tokio::sync::watch::Receiver<polyphony_core::RuntimeSnapshot>,
    _command_tx: tokio::sync::mpsc::UnboundedSender<polyphony_orchestrator::RuntimeCommand>,
    _log_buffer: LogBuffer,
) -> Result<(), TuiError> {
    Err(TuiError::Disabled("current"))
}

#[cfg(feature = "tui-next")]
async fn run_next(
    snapshot_rx: tokio::sync::watch::Receiver<polyphony_core::RuntimeSnapshot>,
    command_tx: tokio::sync::mpsc::UnboundedSender<polyphony_orchestrator::RuntimeCommand>,
) -> Result<(), TuiError> {
    polyphony_tui_next::run(snapshot_rx, command_tx)
        .await
        .map_err(|error| match error {
            polyphony_tui_next::Error::Io(error) => TuiError::Io(error),
        })
}

#[cfg(not(feature = "tui-next"))]
async fn run_next(
    _snapshot_rx: tokio::sync::watch::Receiver<polyphony_core::RuntimeSnapshot>,
    _command_tx: tokio::sync::mpsc::UnboundedSender<polyphony_orchestrator::RuntimeCommand>,
) -> Result<(), TuiError> {
    Err(TuiError::Disabled("next"))
}

#[cfg(feature = "tui")]
pub(crate) fn prompt_workflow_initialization(
    workflow_path: &std::path::Path,
) -> Result<bool, TuiError> {
    polyphony_tui::prompt_workflow_initialization(workflow_path).map_err(|error| match error {
        polyphony_tui::Error::Io(error) => TuiError::Io(error),
    })
}

#[cfg(not(feature = "tui"))]
#[derive(Clone, Default)]
pub(crate) struct LogBuffer {
    lines: Arc<Mutex<VecDeque<String>>>,
}

#[cfg(not(feature = "tui"))]
impl LogBuffer {
    pub(crate) fn from_lines(lines: Vec<String>) -> Self {
        Self {
            lines: Arc::new(Mutex::new(lines.into())),
        }
    }

    #[cfg_attr(not(feature = "tracing"), allow(dead_code))]
    pub(crate) fn push_line(&self, line: String) {
        lock_or_recover(&self.lines).push_back(line);
    }

    pub(crate) fn drain_oldest_first(&self) -> Vec<String> {
        lock_or_recover(&self.lines).drain(..).collect()
    }
}

#[cfg(not(feature = "tui"))]
pub(crate) fn prompt_workflow_initialization(
    _workflow_path: &std::path::Path,
) -> Result<bool, TuiError> {
    Err(TuiError::Disabled("current"))
}

#[cfg(not(feature = "tui"))]
fn lock_or_recover<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poison| poison.into_inner())
}
