use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

const STARTING: u8 = 0;
const READY: u8 = 1;
const SHUTTING_DOWN: u8 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadinessState {
    Starting,
    Ready,
    ShuttingDown,
}

impl ReadinessState {
    fn from_code(code: u8) -> Self {
        match code {
            READY => ReadinessState::Ready,
            SHUTTING_DOWN => ReadinessState::ShuttingDown,
            _ => ReadinessState::Starting,
        }
    }
}

impl fmt::Display for ReadinessState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            ReadinessState::Starting => "starting",
            ReadinessState::Ready => "ready",
            ReadinessState::ShuttingDown => "shutting_down",
        };
        f.write_str(name)
    }
}

#[derive(Clone)]
pub struct Readiness(Arc<AtomicU8>);

impl Readiness {
    pub fn new() -> Self {
        Self(Arc::new(AtomicU8::new(STARTING)))
    }

    pub fn set_ready(&self) {
        self.0.store(READY, Ordering::SeqCst);
    }

    pub fn set_not_ready(&self) {
        self.0.store(SHUTTING_DOWN, Ordering::SeqCst);
    }

    pub fn state(&self) -> ReadinessState {
        ReadinessState::from_code(self.0.load(Ordering::SeqCst))
    }
}

impl Default for Readiness {
    fn default() -> Self {
        Self::new()
    }
}
