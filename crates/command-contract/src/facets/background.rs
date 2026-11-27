use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use crate::outcome::BackgroundCommandResult;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BackgroundRequest {
    pub workspace: PathBuf,
    pub language: String,
    pub updater: PathBuf,
    pub argument: String,
}

pub type BackgroundCancel = Arc<AtomicBool>;

pub type BackgroundJob = fn(BackgroundRequest, BackgroundCancel) -> BackgroundCommandResult;

pub trait CommandBackgroundContext {
    fn start(
        &mut self,
        command: &'static str,
        job: BackgroundJob,
        request: BackgroundRequest,
    ) -> Result<String, String>;

    fn cancel(&mut self, command: &'static str) -> Result<String, String>;
}
