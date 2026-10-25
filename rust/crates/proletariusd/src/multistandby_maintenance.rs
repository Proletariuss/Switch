use crate::multistandby_native_service::Service;
use std::{sync::Arc, time::Duration};
use tokio::time::{MissedTickBehavior, interval};

const TICK_INTERVAL: Duration = Duration::from_secs(1);

/// Signals retained native Profile workers. Their bounded queues coalesce
/// maintenance; Service shutdown separately joins any accepted outbox work.
pub(crate) async fn run(service: Arc<Service>, mut shutdown: tokio::sync::watch::Receiver<bool>) {
    let mut ticker = interval(TICK_INTERVAL);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    while wait_for_tick_or_shutdown(&mut ticker, &mut shutdown).await {
        service.maintenance();
    }
}

async fn wait_for_tick_or_shutdown(
    ticker: &mut tokio::time::Interval,
    shutdown: &mut tokio::sync::watch::Receiver<bool>,
) -> bool {
    loop {
        if *shutdown.borrow() || shutdown.has_changed().is_err() {
            return false;
        }
        tokio::select! {
            _ = shutdown.changed() => {},
            _ = ticker.tick() => return !*shutdown.borrow(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn idle_maintenance_scheduler_exits_promptly_when_signaled() {
        let mut ticker = interval(Duration::from_secs(60));
        let (sender, mut shutdown) = tokio::sync::watch::channel(false);
        // Consume interval's immediate first tick before testing the idle wait.
        assert!(wait_for_tick_or_shutdown(&mut ticker, &mut shutdown).await);
        sender.send(true).unwrap();
        assert!(
            !tokio::time::timeout(
                Duration::from_millis(100),
                wait_for_tick_or_shutdown(&mut ticker, &mut shutdown),
            )
            .await
            .expect("shutdown signal should wake the scheduler")
        );
    }
}
