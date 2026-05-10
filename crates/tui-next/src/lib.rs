mod app;
mod command_palette;
mod detail;
mod event_loop;
mod format;
mod render;
mod rows;
mod status;
mod theme;

use std::io;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("io error: {0}")]
    Io(#[from] io::Error),
}

pub use event_loop::run;
