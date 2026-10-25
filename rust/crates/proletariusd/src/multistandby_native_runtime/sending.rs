use super::*;
use crate::{
    native_registered_session::Control,
    native_sms_originating::{Phase, Receipt, TextSend},
};
impl Manager {
    pub(super) async fn flush(
        &self,
        id: &str,
        session: &NativeMultiSession,
    ) -> Result<(), Failure> {
        // Each maintenance tick is bounded. A claim is committed before its one
        // submit; no error path can return that claim to queued or resubmit it.
        for _ in 0..32 {
            if !self.send_readiness(id).channel_ready {
                return Ok(());
            }
            let attempt = token()?;
            let claim = self
                .apply(
                    "claim-next",
                    json!({"event_id":attempt,"profile_id":id,"attempt_token":attempt}),
                )
                .await?;
            if claim["message"].is_null() {
                return Ok(());
            }
            let message = &claim["message"];
            let batch = format!("native_multi_{attempt}");
            let command = TextSend {
                generation: session.generation(),
                batch_id: batch.clone(),
                recipient: message["phone"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned()
                    .into(),
                text: message["body"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned()
                    .into(),
                request_status_report: true,
            };
            let receipt = tokio::time::timeout(Duration::from_secs(40), async {
                let (reply, wait) = oneshot::channel();
                session
                    .controls()
                    .send(Control::SendText {
                        input: command,
                        reply,
                    })
                    .await
                    .map_err(|_| ())?;
                let mut receipt = wait.await.map_err(|_| ())?.map_err(|_| ())?;
                loop {
                    if receipt.generation != session.generation()
                        || receipt.batch_id != batch
                        || receipt.total_parts == 0
                        || receipt.accepted_parts > receipt.total_parts
                    {
                        return Err(());
                    }
                    if !matches!(receipt.phase, Phase::Prepared | Phase::AwaitingResponse) {
                        return Ok(receipt);
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    let (reply, wait) = oneshot::channel();
                    session
                        .controls()
                        .send(Control::SmsStatus {
                            generation: session.generation(),
                            batch_id: batch.clone(),
                            reply,
                        })
                        .await
                        .map_err(|_| ())?;
                    receipt = wait.await.map_err(|_| ())?.map_err(|_| ())?.ok_or(())?;
                }
            })
            .await
            .ok()
            .and_then(Result::ok);
            let (state, error, sip) = classify(receipt.as_ref());
            self.apply("outcome",json!({"event_id":token()?,"profile_id":id,"message_id":message["message_id"],"attempt_token":attempt,"state":state,"error":error,"sip_code":sip})).await?;
        }
        Ok(())
    }
}
fn classify(receipt: Option<&Receipt>) -> (&'static str, &'static str, Option<u16>) {
    match receipt {
        Some(r)
            if r.phase == Phase::Accepted
                && r.accepted_parts == r.total_parts
                && !r.outcome_unknown() =>
        {
            ("accepted", "", r.sip_status)
        }
        Some(r) if matches!(r.phase, Phase::Rejected | Phase::Aborted) && !r.outcome_unknown() => {
            ("failed", r.error.unwrap_or("sms_rejected"), r.sip_status)
        }
        Some(r) => (
            "unknown",
            r.error.unwrap_or("sms_outcome_unknown"),
            r.sip_status,
        ),
        None => ("unknown", "sms_outcome_unknown", None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn uncertain_or_partially_accepted_claim_is_never_marked_failed_or_replayable() {
        let mut receipt = Receipt {
            generation: crate::native_vowifi_supervisor::SessionGeneration::from_value(1).unwrap(),
            batch_id: "batch".into(),
            phase: Phase::Rejected,
            accepted_parts: 1,
            total_parts: 2,
            sip_status: Some(500),
            error: None,
            rp_results: vec![],
        };
        assert_eq!(classify(Some(&receipt)).0, "unknown");
        receipt.phase = Phase::Partial;
        assert_eq!(classify(Some(&receipt)).0, "unknown");
        receipt.phase = Phase::Accepted;
        assert_eq!(classify(Some(&receipt)).0, "unknown");
        receipt.accepted_parts = 2;
        assert_eq!(classify(Some(&receipt)).0, "accepted");
        receipt.phase = Phase::Rejected;
        receipt.accepted_parts = 0;
        assert_eq!(classify(Some(&receipt)).0, "failed");
        assert_eq!(classify(None).0, "unknown");
    }
}
