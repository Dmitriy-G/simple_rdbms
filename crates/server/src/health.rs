use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Clone)]
pub struct Readiness(Arc<AtomicBool>);

impl Readiness {
    pub fn new() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }

    pub fn set_ready(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_ready(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

impl Default for Readiness {
    fn default() -> Self {
        Self::new()
    }
}
