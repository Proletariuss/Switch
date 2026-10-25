//! Overview cache belongs to one physical USB owner, never a Go generation.
use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EsimSnapshotKey {
    pub(super) producer: String,
    pub(super) epoch: u64,
    pub(super) session: String,
    pub(super) revision: u64,
}
#[derive(Clone)]
pub(crate) struct EsimRefreshBinding {
    producer: String,
    epoch: u64,
    session: String,
    identity_generation: u64,
}
impl EsimSnapshotKey {
    fn same_transport(&self, state: &State) -> bool {
        !state.stopping
            && !state.revoking
            && !state.session.is_empty()
            && self.producer == state.producer
            && self.epoch == state.epoch
            && self.session == state.session
    }
    pub(super) fn current(&self, state: &State) -> bool {
        self.same_transport(state) && self.revision == state.esim_snapshot.revision
    }
}
#[derive(Clone)]
struct Entry {
    key: EsimSnapshotKey,
    overview: Value,
}
#[derive(Default)]
pub(super) struct Cache {
    revision: u64,
    live: Option<Entry>,
    last_good: Option<Entry>,
}
impl Cache {
    pub(super) fn invalidate(&mut self, keep_last_good: bool) {
        // Saturation cannot make an old transaction current: no further cache
        // publication is admitted once the monotonic revision is exhausted.
        self.revision = self.revision.saturating_add(1);
        self.live = None;
        if !keep_last_good {
            self.last_good = None;
        }
    }
    pub(super) fn identity_observed(&mut self, iccid: &str) {
        let cached = self.live.as_ref().or(self.last_good.as_ref());
        let active = cached
            .and_then(|entry| entry.overview["profiles"].as_array())
            .into_iter()
            .flatten()
            .flat_map(|group| group["profiles"].as_array().into_iter().flatten())
            .find(|profile| profile["state"].as_i64() == Some(1))
            .and_then(|profile| profile["iccid"].as_str());
        if active.is_some_and(|old| {
            old.trim_end_matches(['F', 'f']).trim() != iccid.trim_end_matches(['F', 'f']).trim()
        }) {
            self.invalidate(false);
        }
    }
}
impl Owner {
    pub(crate) fn esim_mutation_identity(&self) -> Option<(String, String, u64)> {
        let state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        (!state.stopping && !state.revoking && !state.session.is_empty()).then(|| {
            (
                format!("usb:{}:{}", state.producer, state.epoch),
                format!("usb-session:{}", state.session),
                state.identity_generation,
            )
        })
    }

    pub(crate) fn capture_esim_refresh_binding(&self) -> Option<EsimRefreshBinding> {
        let state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        (!state.stopping && !state.revoking && !state.session.is_empty()).then(|| {
            EsimRefreshBinding {
                producer: state.producer.clone(),
                epoch: state.epoch,
                session: state.session.clone(),
                identity_generation: state.identity_generation,
            }
        })
    }
    pub(crate) fn esim_refresh_binding_current(&self, binding: &EsimRefreshBinding) -> bool {
        let state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        !state.stopping
            && !state.revoking
            && state.producer == binding.producer
            && state.epoch == binding.epoch
            && state.session == binding.session
            && state.identity_generation == binding.identity_generation
    }
    pub(crate) async fn esim_snapshot_guard(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.inner.esim_snapshot_gate.lock().await
    }
    pub(crate) fn esim_snapshot_current(&self, key: &EsimSnapshotKey) -> bool {
        key.current(&self.inner.state.lock().unwrap_or_else(|e| e.into_inner()))
    }
    /// The caller's journal commit runs under the same owner-state lock that
    /// publishes a new physical session or revokes an eSIM snapshot revision.
    /// Never await or perform AT I/O from this callback.
    fn with_current_esim_snapshot<T>(
        &self,
        key: &EsimSnapshotKey,
        commit: impl FnOnce(&mut State) -> T,
    ) -> Option<T> {
        let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        key.current(&state).then(|| commit(&mut state))
    }
    pub(crate) fn cached_esim_overview(&self, stale: bool) -> Option<(EsimSnapshotKey, Value)> {
        let state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        let entry = if stale {
            state.esim_snapshot.last_good.as_ref()
        } else {
            state.esim_snapshot.live.as_ref()
        }?;
        (if stale {
            entry.key.same_transport(&state)
        } else {
            entry.key.current(&state)
        })
        .then(|| (entry.key.clone(), entry.overview.clone()))
    }
    pub(crate) fn invalidate_esim_overview(&self) {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .esim_snapshot
            .invalidate(false);
    }
    fn esim_snapshot_key(&self) -> Result<EsimSnapshotKey, Failure> {
        let state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        check(&state, &state.producer)?;
        if state.session.is_empty() || state.esim_snapshot.revision == u64::MAX {
            return Err(Failure::stale());
        }
        Ok(EsimSnapshotKey {
            producer: state.producer.clone(),
            epoch: state.epoch,
            session: state.session.clone(),
            revision: state.esim_snapshot.revision,
        })
    }
    pub(crate) async fn native_esim_overview(
        &self,
        refresh: bool,
    ) -> Result<(EsimSnapshotKey, Value), Failure> {
        if !refresh && let Some(cached) = self.cached_esim_overview(false) {
            return Ok(cached);
        }
        if refresh {
            self.inner
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .esim_snapshot
                .invalidate(true);
        }
        let owner = self.clone();
        let (key, read) = self
            .esim_transaction_mode(
                move |transport| {
                    let key = owner.esim_snapshot_key()?;
                    let read = crate::module_esim::read_overview_with_identities(transport)?;
                    Ok((key, read))
                },
                false,
            )
            .await?;
        let overview = read.value;
        self.with_current_esim_snapshot(&key, |state| {
            // Commit the recovery decision before publishing a refreshed
            // view, under the exact USB owner/session/revision fence.
            if refresh
                && !crate::reader_esim_download::reconcile_module_overview_at_commit(
                    &overview,
                    &read.complete_card_identities,
                )
            {
                return Err(Failure::new(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "esim_operation_journal_failed",
                    "卡片信息已读取，但无法安全保存上次操作的核对状态",
                ));
            }
            let entry = Entry {
                key: key.clone(),
                overview: overview.clone(),
            };
            state.esim_snapshot.live = Some(entry.clone());
            state.esim_snapshot.last_good = Some(entry);
            Ok(())
        })
        .ok_or_else(Failure::stale)??;
        Ok((key, overview))
    }
}

#[cfg(test)]
mod tests;
