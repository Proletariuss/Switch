//! Native startup observes actual actor completion, not the compatibility RPC
//! response budget. The actor retains every submitted probe through port close.
use super::*;

impl ModemActor {
    /// Probe the legacy, serial-first candidate list and await its actual
    /// completion. Only `at_port_not_found` confirms an exhausted scan; busy,
    /// stopped and enumeration errors must not authorize USB fallback.
    ///
    /// Dropping this observer does not cancel the retained blocking waiter or
    /// the actor's submitted I/O. It provides no completion receipt. Shutdown
    /// must still join the actor's real close acknowledgement.
    pub(crate) async fn discover_at_port_confirmed(&self) -> Result<DiscoveredATPort, ActorError> {
        self.discover_candidates_confirmed(None).await
    }

    #[cfg(test)]
    async fn discover_at_port_using(
        &self,
        enumerate: impl FnOnce() -> Result<Vec<String>, ActorError>,
    ) -> Result<DiscoveredATPort, ActorError> {
        self.discover_candidates_confirmed(Some(enumerate()?)).await
    }

    async fn discover_candidates_confirmed(
        &self,
        candidates: Option<Vec<String>>,
    ) -> Result<DiscoveredATPort, ActorError> {
        let (reply, result) = mpsc::sync_channel(1);
        self.client
            .sender
            .try_send(Command::DiscoverATPort { candidates, reply })
            .map_err(|error| match error {
                mpsc::TrySendError::Full(_) => {
                    ActorError::new("serial_actor_busy", "serial actor request capacity is full")
                }
                mpsc::TrySendError::Disconnected(_) => {
                    ActorError::new("serial_actor_stopped", "serial actor has stopped")
                }
            })?;
        // Enqueue before the first suspension point. Cancellation cannot leave
        // an enumeration task that submits a probe after a later Close command.
        tokio::task::spawn_blocking(move || {
            result
                .recv()
                .map_err(|_| ActorError::new("serial_actor_stopped", "serial actor has stopped"))?
        })
        .await
        .map_err(|_| ActorError::new("serial_actor_stopped", "serial discovery task failed"))?
    }
}

#[cfg(test)]
mod tests;
