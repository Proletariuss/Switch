use crate::{archive_io, multistandby_native_service::Service, multistandby_store};
use serde_json::Value;
use std::{sync::Arc, time::Duration};
use tokio::time::{MissedTickBehavior, interval};

const POLL_INTERVAL: Duration = Duration::from_secs(5);

fn has_keep_online_intent(catalog: &Value) -> bool {
    catalog["desired"].as_array().is_some_and(|desired| {
        desired
            .iter()
            .any(|entry| entry["desired_state"].as_str() == Some("keep_online"))
    })
}

/// The facade owns exact reader-generation recovery and Store recovery policy.
/// The timer only decides when durable keep-online intent needs observation.
pub(crate) async fn run(
    store: multistandby_store::Store,
    service: Arc<Service>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) {
    let mut ticker = interval(POLL_INTERVAL);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    while wait_for_tick_or_shutdown(&mut ticker, &mut shutdown).await {
        let catalog_store = store.clone();
        let intent = archive_io::run(move || catalog_store.runtime_catalog())
            .await
            .is_ok_and(|catalog| has_keep_online_intent(&catalog));
        if *shutdown.borrow() || shutdown.has_changed().is_err() {
            return;
        }
        if !intent {
            continue;
        }
        // Never abort a submitted native inventory at shutdown: its retained
        // task must join real APDUs before this scheduler acknowledges exit.
        let _ = service.snapshot("", true, true).await;
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
mod tests;
