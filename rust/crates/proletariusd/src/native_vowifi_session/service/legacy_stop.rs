//! Unknown compatibility capabilities remain fenced until their real owner
//! exits. Go has no ordinary IMS execution entry point and is never asked to
//! manufacture a stop acknowledgement for a native session.
use super::*;
impl Service {
    pub(crate) async fn stop_global(
        &self,
        deadline: tokio::time::Instant,
    ) -> Result<(), &'static str> {
        if self
            .0
            .resources
            .ordinary_admission
            .current_go_claim()
            .map_err(|_| "ordinary_owner_unavailable")?
            .is_some()
        {
            return Err("legacy_stop_unconfirmed");
        }
        self.stop(None, deadline).await
    }
}
