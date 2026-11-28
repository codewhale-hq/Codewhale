pub struct QueuedDrafts {
    pub editing: Option<String>,
    pub waiting: Vec<String>,
}

pub struct ParkedDraft {
    pub timestamp: String,
    pub text: String,
}

pub trait CommandComposerContext {
    fn queue_previews(&self) -> QueuedDrafts;
    fn editing_queue(&self) -> bool;
    fn edit_queued(&mut self, index: usize, status: String) -> bool;
    fn drop_queued(&mut self, index: usize) -> bool;
    fn clear_queued(&mut self) -> bool;
    fn parked_drafts(&self) -> Vec<ParkedDraft>;
    fn restore_parked(&mut self) -> Option<(String, usize)>;
    fn clear_parked(&mut self) -> Result<usize, String>;
}
