//! Rust-owned durable retry/IMS lease timers dispatch directly to the native
//! owner. Acceptance retires only the exact notification, never session work.
use crate::{
    multistandby_native_service::{DueDelivery, Service},
    multistandby_runtime_report::{Journal, RegistrationDue, RetryDue},
};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::time::{MissedTickBehavior, interval};
const POLL_INTERVAL: Duration = Duration::from_millis(250);

pub(crate) async fn run(
    journal: Arc<Mutex<Journal>>,
    service: Arc<Service>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) {
    let mut ticker = interval(POLL_INTERVAL);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    while wait_for_tick_or_shutdown(&mut ticker, &mut shutdown).await {
        let now = now_ms();
        let registration = journal
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .next_due_registration(now);
        if let Some(due) = registration {
            let delivery = service.dispatch_registration_due(due.clone());
            complete_registration(&journal, &due, delivery, now_ms());
            continue;
        }
        let due = journal
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .next_due_retry(now);
        if let Some(due) = due {
            // Pre-token records cannot identify a live native attempt. Retire
            // them as stale; never fabricate a token or restart a new attempt.
            let delivery = if due.attempt_id.is_empty() {
                DueDelivery::Stale
            } else {
                service.dispatch_retry_due(due.clone())
            };
            complete_retry(&journal, &due, delivery, now_ms());
        }
    }
}
fn complete_retry(journal: &Mutex<Journal>, due: &RetryDue, delivery: DueDelivery, now: i64) {
    if matches!(delivery, DueDelivery::Accepted | DueDelivery::Stale) {
        let _ = journal
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .complete_retry(due, now);
    }
}
fn complete_registration(
    journal: &Mutex<Journal>,
    due: &RegistrationDue,
    delivery: DueDelivery,
    now: i64,
) {
    if matches!(delivery, DueDelivery::Accepted | DueDelivery::Stale) {
        let _ = journal
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .complete_registration(due, now);
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

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

#[cfg(test)]
mod tests;
