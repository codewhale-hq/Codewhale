use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

#[derive(Clone, Default)]
pub struct EventBroker {
    paused: Arc<AtomicBool>,
}

impl EventBroker {
    pub fn new() -> Self {
        Self {
            paused: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn pause_events(&self) {
        self.paused.store(true, Ordering::SeqCst);
    }

    pub fn resume_events(&self) {
        self.paused.store(false, Ordering::SeqCst);
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
    }
}
