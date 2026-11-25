use std::future::Future;
use std::pin::Pin;

use crate::outcome::BackgroundCommandResult;

pub type BackgroundCommandWork =
    Pin<Box<dyn Future<Output = BackgroundCommandResult> + Send + 'static>>;

pub trait CommandBackgroundContext {
    fn start(
        &mut self,
        command: &'static str,
        work: BackgroundCommandWork,
    ) -> Result<String, String>;

    fn cancel(&mut self, command: &'static str) -> Result<String, String>;
}
