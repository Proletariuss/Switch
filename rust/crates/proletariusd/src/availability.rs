use std::time::Duration;

use tokio::sync::watch;
use tokio::time::timeout;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BackendPhase {
    Starting,
    Ready,
    Restarting,
    Failed,
}

impl BackendPhase {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Ready => "ready",
            Self::Restarting => "restarting",
            Self::Failed => "failed",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct BackendRuntime {
    pub(crate) phase: BackendPhase,
    pub(crate) generation: u64,
    pub(crate) restart_attempts: u32,
}

#[derive(Clone)]
pub(crate) struct BackendAvailability {
    sender: watch::Sender<BackendRuntime>,
    module_recovery: crate::module_recovery::Gate,
}

impl BackendAvailability {
    pub(crate) fn new() -> Self {
        let (sender, _) = watch::channel(BackendRuntime {
            phase: BackendPhase::Starting,
            generation: 1,
            restart_attempts: 0,
        });
        Self {
            sender,
            module_recovery: Default::default(),
        }
    }

    pub(crate) fn with_module_recovery(gate: crate::module_recovery::Gate) -> Self {
        let mut availability = Self::new();
        availability.module_recovery = gate;
        availability
    }

    pub(crate) fn module_recovery(&self) -> &crate::module_recovery::Gate {
        &self.module_recovery
    }

    pub(crate) fn snapshot(&self) -> BackendRuntime {
        *self.sender.borrow()
    }

    pub(crate) fn mark_ready(&self) {
        self.update(|runtime| runtime.phase = BackendPhase::Ready);
    }

    pub(crate) fn mark_starting(&self) {
        self.update(|runtime| runtime.phase = BackendPhase::Starting);
    }

    pub(crate) fn mark_failed(&self) {
        self.update(|runtime| runtime.phase = BackendPhase::Failed);
    }

    pub(crate) fn mark_restarting(&self, restart_attempts: u32) {
        self.update(|runtime| {
            runtime.phase = BackendPhase::Restarting;
            runtime.restart_attempts = restart_attempts;
        });
    }

    pub(crate) fn child_started(&self) {
        self.update(|runtime| {
            runtime.generation = runtime.generation.saturating_add(1);
            runtime.phase = BackendPhase::Restarting;
        });
    }

    pub(crate) fn reset_restart_attempts(&self) {
        self.update(|runtime| runtime.restart_attempts = 0);
    }

    pub(crate) async fn wait_until_ready(&self, wait: Duration) -> bool {
        let mut receiver = self.sender.subscribe();
        if receiver.borrow().phase == BackendPhase::Ready {
            return true;
        }
        if receiver.borrow().phase == BackendPhase::Failed {
            return false;
        }
        timeout(wait, async move {
            loop {
                if receiver.changed().await.is_err() {
                    return false;
                }
                if receiver.borrow().phase == BackendPhase::Ready {
                    return true;
                }
                if receiver.borrow().phase == BackendPhase::Failed {
                    return false;
                }
            }
        })
        .await
        .unwrap_or(false)
    }

    fn update(&self, update: impl FnOnce(&mut BackendRuntime)) {
        self.sender.send_modify(update);
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{BackendAvailability, BackendPhase};

    #[tokio::test]
    async fn waiting_read_wakes_when_backend_becomes_ready() {
        let availability = BackendAvailability::new();
        let marker = availability.clone();
        let task = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            marker.mark_ready();
        });
        assert!(availability.wait_until_ready(Duration::from_secs(1)).await);
        task.await.expect("marker task");
    }

    #[tokio::test]
    async fn waiting_read_is_bounded_when_backend_stays_unavailable() {
        let availability = BackendAvailability::new();
        assert!(
            !availability
                .wait_until_ready(Duration::from_millis(5))
                .await
        );
    }

    #[test]
    fn restart_preserves_attempts_and_advances_generation() {
        let availability = BackendAvailability::new();
        availability.mark_ready();
        availability.mark_starting();
        assert_eq!(availability.snapshot().phase, BackendPhase::Starting);
        availability.mark_ready();
        availability.mark_restarting(2);
        availability.child_started();
        let restarting = availability.snapshot();
        assert_eq!(restarting.phase, BackendPhase::Restarting);
        assert_eq!(restarting.generation, 2);
        assert_eq!(restarting.restart_attempts, 2);
        availability.mark_ready();
        assert_eq!(availability.snapshot().phase, BackendPhase::Ready);
        availability.reset_restart_attempts();
        assert_eq!(availability.snapshot().restart_attempts, 0);
    }

    #[tokio::test]
    async fn failed_child_wakes_waiting_compatibility_reads_without_disabling_native_owners() {
        let availability = BackendAvailability::new();
        let waiter = availability.clone();
        let task =
            tokio::spawn(async move { waiter.wait_until_ready(Duration::from_secs(60)).await });
        tokio::task::yield_now().await;
        availability.mark_failed();
        assert!(
            !tokio::time::timeout(Duration::from_secs(1), task)
                .await
                .unwrap()
                .unwrap()
        );
        assert_eq!(availability.snapshot().phase.as_str(), "failed");
        assert!(!availability.wait_until_ready(Duration::from_secs(60)).await);
        availability.child_started();
        availability.mark_ready();
        assert!(availability.wait_until_ready(Duration::ZERO).await);
    }
}
