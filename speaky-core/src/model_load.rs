use std::sync::atomic::{AtomicU8, Ordering};
use tokio::sync::Notify;

const LOADING: u8 = 0;
const CANCELLED: u8 = 1;
const READY: u8 = 2;
pub const CANCELLED_MESSAGE: &str = "已停止加载模型。";

/// Controls one attempt to load a local recognition model.
#[derive(Default)]
pub struct ModelLoad {
    state: AtomicU8,
    stopped: Notify,
}

impl ModelLoad {
    pub fn cancel(&self) -> bool {
        let cancelled = self
            .state
            .compare_exchange(LOADING, CANCELLED, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok();
        if cancelled {
            self.stopped.notify_waiters();
        }
        cancelled
    }

    pub fn is_loading(&self) -> bool {
        self.state.load(Ordering::Relaxed) == LOADING
    }

    pub fn is_cancelled(&self) -> bool {
        self.state.load(Ordering::Relaxed) == CANCELLED
    }

    pub fn check(&self) -> Result<(), String> {
        if self.is_cancelled() {
            return Err(CANCELLED_MESSAGE.into());
        }
        Ok(())
    }

    pub(crate) fn complete(&self) -> Result<(), String> {
        match self
            .state
            .compare_exchange(LOADING, READY, Ordering::Relaxed, Ordering::Relaxed)
        {
            Err(CANCELLED) => Err(CANCELLED_MESSAGE.into()),
            _ => Ok(()),
        }
    }

    pub(crate) async fn cancelled(&self) {
        let stopped = self.stopped.notified();
        if !self.is_cancelled() {
            stopped.await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_and_readiness_are_mutually_exclusive() {
        let stopped = ModelLoad::default();
        assert!(stopped.cancel());
        assert!(stopped.complete().is_err());

        let ready = ModelLoad::default();
        assert!(ready.complete().is_ok());
        assert!(!ready.cancel());
        assert!(!ready.is_cancelled());
    }
}
