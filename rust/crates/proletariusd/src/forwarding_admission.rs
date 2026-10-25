//! Rust-owned N-wait SMS forwarding producer.
//!
//! This consumer is gated on the durable Swift handoff. It reads the same
//! atomic archive as the Rust N-wait API, persists its own cursor, and advances
//! only after the provider queue confirms durable admission.

use std::time::Duration;

use proletarius_api_contract::SmsForwardingHandoffV1;
use proletarius_forwarding::ForwardingMessage;
use tokio::time::{MissedTickBehavior, interval};

const POLL_INTERVAL: Duration = Duration::from_secs(5);

trait Runtime: Sync {
    async fn current_handoff(&self) -> Result<Option<SmsForwardingHandoffV1>, String>;
    async fn forwarding_enabled(&self) -> Result<bool, String>;
    async fn current_progress(
        &self,
    ) -> Result<Option<crate::forwarding_admission_store::Progress>, String>;
    async fn current_revision(&self) -> Result<u64, String>;
    async fn forwarding_page(&self, cursor: u64) -> Result<(Vec<ForwardingMessage>, u64), String>;
    async fn import_handoff(
        &self,
        handoff: SmsForwardingHandoffV1,
        revision: u64,
        enabled: bool,
    ) -> Result<(), String>;
    async fn advance(&self, handoff_id: String, cursor: u64, enabled: bool) -> Result<(), String>;
    async fn enqueue(&self, messages: &[ForwardingMessage]) -> Result<bool, String>;
}

#[derive(Clone)]
struct Stores {
    messages: crate::multistandby_store::Store,
    handoffs: crate::forwarding_handoff_store::Store,
    progress: crate::forwarding_admission_store::Store,
    forwarding: crate::sms_forwarding::Service,
}

impl Runtime for Stores {
    async fn current_handoff(&self) -> Result<Option<SmsForwardingHandoffV1>, String> {
        let store = self.handoffs.clone();
        crate::archive_io::run(move || store.current())
            .await
            .map_err(|error| error.to_string())
    }

    async fn forwarding_enabled(&self) -> Result<bool, String> {
        self.forwarding
            .admission_enabled()
            .await
            .map_err(str::to_owned)
    }

    async fn current_progress(
        &self,
    ) -> Result<Option<crate::forwarding_admission_store::Progress>, String> {
        let store = self.progress.clone();
        crate::archive_io::run(move || store.current())
            .await
            .map_err(|error| error.to_string())
    }

    async fn current_revision(&self) -> Result<u64, String> {
        let store = self.messages.clone();
        crate::archive_io::run(move || store.current_revision())
            .await
            .map_err(|error| error.to_string())
    }

    async fn forwarding_page(&self, cursor: u64) -> Result<(Vec<ForwardingMessage>, u64), String> {
        let store = self.messages.clone();
        crate::archive_io::run(move || store.forwarding_page(cursor))
            .await
            .map_err(|error| error.to_string())
    }

    async fn import_handoff(
        &self,
        handoff: SmsForwardingHandoffV1,
        revision: u64,
        enabled: bool,
    ) -> Result<(), String> {
        let store = self.progress.clone();
        crate::archive_io::run(move || store.import_handoff(&handoff, revision, enabled))
            .await
            .map_err(|error| error.to_string())
            .map(|_| ())
    }

    async fn advance(&self, handoff_id: String, cursor: u64, enabled: bool) -> Result<(), String> {
        let store = self.progress.clone();
        crate::archive_io::run(move || store.advance(&handoff_id, cursor, enabled))
            .await
            .map_err(|error| error.to_string())
            .map(|_| ())
    }

    async fn enqueue(&self, messages: &[ForwardingMessage]) -> Result<bool, String> {
        self.forwarding
            .enqueue_admitted(messages)
            .await
            .map_err(|_| "queue admission was not durable".to_owned())
    }
}

pub(crate) async fn run(
    messages: crate::multistandby_store::Store,
    handoffs: crate::forwarding_handoff_store::Store,
    progress: crate::forwarding_admission_store::Store,
    forwarding: crate::sms_forwarding::Service,
) {
    let runtime = Stores {
        messages,
        handoffs,
        progress,
        forwarding,
    };
    let mut ticker = interval(POLL_INTERVAL);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        ticker.tick().await;
        if let Err(error) = poll_once(&runtime).await {
            crate::diagnostics::write_line(format_args!(
                "switchd-rs N-wait forwarding admission: {error}"
            ));
        }
    }
}

async fn poll_once(runtime: &impl Runtime) -> Result<(), String> {
    let Some(handoff) = runtime.current_handoff().await? else {
        return Ok(());
    };

    // An unfinished/failed configuration is not an intentional disable. Keep
    // the cursor and first-enable state unchanged until admission is known.
    let enabled = runtime.forwarding_enabled().await?;
    let Some(current) = runtime.current_progress().await? else {
        let revision = runtime.current_revision().await?;
        runtime.import_handoff(handoff, revision, enabled).await?;
        return Ok(());
    };
    if current.handoff_id != handoff.handoff_id {
        return Err("stored progress belongs to a different handoff".to_owned());
    }

    if !enabled {
        let revision = runtime.current_revision().await?;
        runtime.advance(current.handoff_id, revision, false).await?;
        return Ok(());
    }

    // Preserve the existing no-backfill rule when forwarding was disabled or
    // the prior Swift worker had never established an enabled cursor.
    if current.was_enabled != Some(true) {
        let revision = runtime.current_revision().await?;
        runtime.advance(current.handoff_id, revision, true).await?;
        return Ok(());
    }

    let (page, next_cursor) = runtime.forwarding_page(current.cursor).await?;
    if !page.is_empty() && !runtime.enqueue(&page).await? {
        let revision = runtime.current_revision().await?;
        runtime.advance(current.handoff_id, revision, false).await?;
        return Ok(());
    }

    runtime
        .advance(current.handoff_id, next_cursor, true)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    struct FakeRuntime(Arc<Mutex<FakeState>>);

    struct FakeState {
        handoff: Option<SmsForwardingHandoffV1>,
        progress: Option<crate::forwarding_admission_store::Progress>,
        enabled: bool,
        admission_error: Option<String>,
        revision: u64,
        page: Vec<ForwardingMessage>,
        next_cursor: u64,
        enqueue_result: Result<bool, String>,
        fail_advance_once: bool,
        enqueue_attempts: Vec<Vec<String>>,
        accepted_ids: HashSet<String>,
    }

    impl FakeRuntime {
        fn ready(enabled: bool) -> Self {
            let handoff: SmsForwardingHandoffV1 = serde_json::from_value(serde_json::json!({
                "schema_version": 1,
                "handoff_id": "123e4567-e89b-12d3-a456-426614174000",
                "device_sms": {"accepted_notification_ids": [], "was_enabled": true},
                "nwait_sms": {"cursor": "10", "seeded": true, "was_enabled": true}
            }))
            .unwrap();
            let progress = serde_json::from_value(serde_json::json!({
                "schema_version": 1,
                "handoff_id": handoff.handoff_id,
                "cursor": 10,
                "seeded": true,
                "was_enabled": true
            }))
            .unwrap();
            let page = vec![
                ForwardingMessage::new(
                    "nwait:profile:message",
                    "inbound",
                    "sender",
                    "synthetic body",
                    "2026-10-04T00:00:00Z",
                    "N待",
                )
                .unwrap(),
            ];
            Self(Arc::new(Mutex::new(FakeState {
                handoff: Some(handoff),
                progress: Some(progress),
                enabled,
                admission_error: None,
                revision: 12,
                page,
                next_cursor: 12,
                enqueue_result: Ok(true),
                fail_advance_once: false,
                enqueue_attempts: Vec::new(),
                accepted_ids: HashSet::new(),
            })))
        }

        fn with_state<T>(&self, read: impl FnOnce(&FakeState) -> T) -> T {
            read(&self.0.lock().unwrap())
        }

        fn update(&self, change: impl FnOnce(&mut FakeState)) {
            change(&mut self.0.lock().unwrap());
        }
    }

    impl Runtime for FakeRuntime {
        async fn current_handoff(&self) -> Result<Option<SmsForwardingHandoffV1>, String> {
            Ok(self.with_state(|state| state.handoff.clone()))
        }

        async fn forwarding_enabled(&self) -> Result<bool, String> {
            self.with_state(|state| match &state.admission_error {
                Some(error) => Err(error.clone()),
                None => Ok(state.enabled),
            })
        }

        async fn current_progress(
            &self,
        ) -> Result<Option<crate::forwarding_admission_store::Progress>, String> {
            Ok(self.with_state(|state| state.progress.clone()))
        }

        async fn current_revision(&self) -> Result<u64, String> {
            Ok(self.with_state(|state| state.revision))
        }

        async fn forwarding_page(
            &self,
            _cursor: u64,
        ) -> Result<(Vec<ForwardingMessage>, u64), String> {
            Ok(self.with_state(|state| (state.page.clone(), state.next_cursor)))
        }

        async fn import_handoff(
            &self,
            _handoff: SmsForwardingHandoffV1,
            revision: u64,
            enabled: bool,
        ) -> Result<(), String> {
            self.update(|state| {
                state.progress = Some(
                    serde_json::from_value(serde_json::json!({
                        "schema_version": 1,
                        "handoff_id": "123e4567-e89b-12d3-a456-426614174000",
                        "cursor": revision,
                        "seeded": true,
                        "was_enabled": enabled
                    }))
                    .unwrap(),
                );
            });
            Ok(())
        }

        async fn advance(
            &self,
            handoff_id: String,
            cursor: u64,
            enabled: bool,
        ) -> Result<(), String> {
            let mut state = self.0.lock().unwrap();
            if state.fail_advance_once {
                state.fail_advance_once = false;
                return Err("synthetic durable cursor failure".to_owned());
            }
            let progress = state
                .progress
                .as_mut()
                .ok_or_else(|| "progress is missing".to_owned())?;
            if progress.handoff_id != handoff_id || cursor < progress.cursor {
                return Err("progress conflict".to_owned());
            }
            progress.cursor = cursor;
            progress.was_enabled = Some(enabled);
            Ok(())
        }

        async fn enqueue(&self, messages: &[ForwardingMessage]) -> Result<bool, String> {
            let mut state = self.0.lock().unwrap();
            let ids: Vec<_> = messages
                .iter()
                .map(|message| message.id().to_owned())
                .collect();
            state.enqueue_attempts.push(ids.clone());
            match &state.enqueue_result {
                Ok(true) => {
                    state.accepted_ids.extend(ids);
                    Ok(true)
                }
                Ok(false) => Ok(false),
                Err(error) => Err(error.clone()),
            }
        }
    }

    #[tokio::test]
    async fn uncertain_configuration_preserves_cursor_and_enabled_history_until_retry() {
        for code in [
            "sms_forwarding_configuration_unconfirmed",
            "sms_forwarding_configuration_pending",
            "sms_forwarding_stopping",
            "sms_forwarding_runner_panicked",
            "device_forwarding_admission_unavailable",
        ] {
            let runtime = FakeRuntime::ready(true);
            runtime.update(|state| state.admission_error = Some(code.to_owned()));
            assert_eq!(poll_once(&runtime).await, Err(code.to_owned()));
            runtime.with_state(|state| {
                let progress = state.progress.as_ref().unwrap();
                assert_eq!(progress.cursor, 10);
                assert_eq!(progress.was_enabled, Some(true));
                assert!(state.enqueue_attempts.is_empty());
                assert!(state.accepted_ids.is_empty());
            });
            runtime.update(|state| state.admission_error = None);
            poll_once(&runtime).await.unwrap();
            runtime.with_state(|state| {
                assert_eq!(state.progress.as_ref().unwrap().cursor, 12);
                assert_eq!(
                    state.enqueue_attempts,
                    vec![vec!["nwait:profile:message".to_owned()]]
                );
                assert_eq!(state.accepted_ids.len(), 1);
            });
        }
    }

    #[tokio::test]
    async fn uncertain_configuration_does_not_seed_a_handoff_baseline() {
        let runtime = FakeRuntime::ready(true);
        runtime.update(|state| {
            state.progress = None;
            state.admission_error = Some("sms_forwarding_configuration_unconfirmed".to_owned());
        });
        assert!(poll_once(&runtime).await.is_err());
        assert!(runtime.with_state(|state| state.progress.is_none()));
        runtime.update(|state| state.admission_error = None);
        poll_once(&runtime).await.unwrap();
        assert_eq!(
            runtime.with_state(|state| state.progress.as_ref().unwrap().cursor),
            12
        );
        assert_eq!(
            runtime.with_state(|state| state.progress.as_ref().unwrap().was_enabled),
            Some(true)
        );
    }

    #[tokio::test]
    async fn enqueue_error_keeps_cursor_for_same_id_retry() {
        let runtime = FakeRuntime::ready(true);
        runtime.update(|state| state.enqueue_result = Err("unknown durable outcome".to_owned()));
        assert!(poll_once(&runtime).await.is_err());
        assert_eq!(
            runtime.with_state(|state| state.progress.as_ref().unwrap().cursor),
            10
        );

        runtime.update(|state| state.enqueue_result = Ok(true));
        poll_once(&runtime).await.unwrap();
        assert_eq!(
            runtime.with_state(|state| state.progress.as_ref().unwrap().cursor),
            12
        );
        assert_eq!(
            runtime.with_state(|state| state.enqueue_attempts.clone()),
            vec![vec!["nwait:profile:message".to_owned()]; 2]
        );
        assert_eq!(runtime.with_state(|state| state.accepted_ids.len()), 1);
    }

    #[tokio::test]
    async fn cursor_write_failure_retries_after_idempotent_queue_admission() {
        let runtime = FakeRuntime::ready(true);
        runtime.update(|state| state.fail_advance_once = true);
        assert!(poll_once(&runtime).await.is_err());
        assert_eq!(
            runtime.with_state(|state| state.progress.as_ref().unwrap().cursor),
            10
        );
        assert_eq!(runtime.with_state(|state| state.accepted_ids.len()), 1);

        poll_once(&runtime).await.unwrap();
        assert_eq!(
            runtime.with_state(|state| state.progress.as_ref().unwrap().cursor),
            12
        );
        assert_eq!(runtime.with_state(|state| state.accepted_ids.len()), 1);
        assert_eq!(runtime.with_state(|state| state.enqueue_attempts.len()), 2);
    }

    #[tokio::test]
    async fn disabled_and_reenabled_forwarding_advance_high_water_without_backfill() {
        let runtime = FakeRuntime::ready(false);
        poll_once(&runtime).await.unwrap();
        assert_eq!(
            runtime.with_state(|state| state.progress.as_ref().unwrap().cursor),
            12
        );
        assert_eq!(
            runtime.with_state(|state| state.progress.as_ref().unwrap().was_enabled),
            Some(false)
        );
        assert!(runtime.with_state(|state| state.enqueue_attempts.is_empty()));

        runtime.update(|state| {
            state.enabled = true;
            state.revision = 14;
        });
        poll_once(&runtime).await.unwrap();
        assert_eq!(
            runtime.with_state(|state| state.progress.as_ref().unwrap().cursor),
            14
        );
        assert_eq!(
            runtime.with_state(|state| state.progress.as_ref().unwrap().was_enabled),
            Some(true)
        );
        assert!(runtime.with_state(|state| state.enqueue_attempts.is_empty()));
    }

    #[tokio::test]
    async fn handoff_mismatch_fails_closed_without_admission() {
        let runtime = FakeRuntime::ready(true);
        runtime.update(|state| {
            state.progress.as_mut().unwrap().handoff_id =
                "123e4567-e89b-12d3-a456-426614174001".to_owned();
        });
        assert!(poll_once(&runtime).await.is_err());
        assert!(runtime.with_state(|state| state.enqueue_attempts.is_empty()));
    }
}
