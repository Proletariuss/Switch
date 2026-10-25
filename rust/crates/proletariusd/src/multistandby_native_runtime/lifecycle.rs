use super::*;
use crate::native_vowifi_session::multi::CleanupReceipt;
impl Manager {
    /// The exact reader generation is captured before queueing and rechecked by
    /// the retained command. A delayed reader-loss callback cannot stop a new card.
    pub(crate) fn reader_lost(
        self: &Arc<Self>,
        reader: String,
        generation: u64,
    ) -> Result<Ticket, Failure> {
        self.stop_and_drain(reader, generation)
    }
    pub(crate) fn stop_and_drain(
        self: &Arc<Self>,
        reader: String,
        generation: u64,
    ) -> Result<Ticket, Failure> {
        let permit = self
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| "native_multi_queue_full")?;
        let manager = self.clone();
        let (send, wait) = oneshot::channel();
        tokio::spawn(async move {
            let _permit = permit;
            let _serial = manager.commands.lock().await;
            let result = manager.drain_generation(&reader, generation).await;
            let _ = send.send(result);
        });
        Ok(Ticket(wait))
    }
    async fn drain_generation(&self, reader: &str, generation: u64) -> Result<Value, Failure> {
        let (ids, runtime) = {
            let state = self.state.lock().unwrap();
            if state
                .inventory
                .as_ref()
                .is_none_or(|i| i.reader != reader || i.generation != generation)
            {
                return Ok(json!({"status":"completed","stale":true}));
            }
            (
                state.entries.keys().cloned().collect::<Vec<_>>(),
                state.runtime.as_ref().map(|(_, r)| r.clone()),
            )
        };
        for id in &ids {
            self.cancel_profile(id);
        }
        // Even an individual worker error is not release proof. The runtime
        // must independently join all retained startup/consumer owners and get
        // the pool's exact physical receipt before we discard this generation.
        for id in &ids {
            let _ = self.stop_profile(id).await;
        }
        let receipt = match runtime {
            Some(runtime) => runtime
                .retry_shutdown_receipt()
                .await
                .map_err(|_| "native_multi_cleanup_unconfirmed")?,
            None => CleanupReceipt::NoProfileWork,
        };
        let mut state = self.state.lock().unwrap();
        state.entries.clear();
        state.runtime = None;
        state.inventory = None;
        Ok(
            json!({"status":"completed","card_released":true,"original_profile_restored":receipt==CleanupReceipt::OriginalRestored,"cleanup":match receipt{CleanupReceipt::OriginalRestored=>"original_restored",CleanupReceipt::NoProfileWork=>"no_profile_work",CleanupReceipt::ConfirmedReaderLost=>"confirmed_reader_lost"}}),
        )
    }
}
