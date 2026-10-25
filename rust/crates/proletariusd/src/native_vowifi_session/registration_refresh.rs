//! Credentials remain bound to the original generation and card capability.
use super::*;
impl NativeSessionContext {
    pub(super) fn enable_registration_refresh(
        &self,
        session: &mut RegisteredImsSession,
    ) -> Result<(), String> {
        let card = self.card.clone();
        let isim = self.subscriber_identity.uses_isim();
        session
            .enable_registration_refresh(
                self.subscriber_identity.private_identity().to_owned(),
                Arc::new(move |rand, autn| {
                    let card = card.clone();
                    Box::pin(async move {
                        let result = if isim {
                            card.authenticate_ims(&rand, &autn).await
                        } else {
                            card.authenticate_ike(&rand, &autn).await
                        };
                        result.map(ims_sim_answer).map_err(|_| {
                            std::io::Error::other("session card IMS authentication failed")
                        })
                    })
                }),
            )
            .map_err(|e| e.to_string())
    }
}
