//! REGISTER response waiting on the session's single SIP stream reader.
use crate::{
    RegisterAction, RegisterPhase, RegisterTracker, SipError, SipResponse,
    stream::{SipStreamItem, SipStreamReader},
};
use tokio::{
    io::AsyncRead,
    sync::{mpsc, watch},
    time::Instant,
};

#[derive(Debug)]
pub enum RegisterWaitError {
    SecurityTransportUnavailable,
    Io(std::io::Error),
    Sip(SipError),
    Deadline,
    Cancelled,
    Disconnected,
    EventConsumerUnavailable,
}

struct Pending<'a> {
    tracker: &'a mut RegisterTracker,
    complete: bool,
}
impl Drop for Pending<'_> {
    fn drop(&mut self) {
        if !self.complete {
            self.tracker.phase = RegisterPhase::Failed;
        }
    }
}

/// Wait only for this REGISTER transaction. Incoming requests and other
/// transactions are handed back to the session owner, not consumed/lost.
/// `deadline` is absolute: provisional responses/CRLF cannot extend it.
/// Dropping this future marks the in-progress transaction failed.
pub async fn await_registration<R: AsyncRead + Unpin>(
    reader: &mut SipStreamReader<R>,
    tracker: &mut RegisterTracker,
    deadline: Instant,
    stop: watch::Receiver<bool>,
    events: &mpsc::Sender<SipStreamItem>,
) -> Result<(RegisterAction, SipResponse), RegisterWaitError> {
    await_registration_with_sink(reader, tracker, deadline, stop, |item| {
        events
            .try_send(item)
            .map_err(|_| RegisterWaitError::EventConsumerUnavailable)
    })
    .await
}

/// Keep early requests within their actual registration transport owner.
/// A full queue is terminal; no frame is silently broadcast or relabelled.
pub async fn await_registration_retained<R: AsyncRead + Unpin>(
    reader: &mut SipStreamReader<R>,
    tracker: &mut RegisterTracker,
    deadline: Instant,
    stop: watch::Receiver<bool>,
    events: &mut std::collections::VecDeque<SipStreamItem>,
) -> Result<(RegisterAction, SipResponse), RegisterWaitError> {
    await_registration_with_sink(reader, tracker, deadline, stop, |item| {
        let length = |item: &SipStreamItem| match item {
            SipStreamItem::Message(frame) => frame.len(),
            SipStreamItem::KeepAliveLine => 2,
        };
        if events.len() >= 32
            || length(&item) > crate::MAX_SIP_MESSAGE_LENGTH
            || events.iter().map(length).sum::<usize>() + length(&item) > 256 * 1024
        {
            return Err(RegisterWaitError::EventConsumerUnavailable);
        }
        events.push_back(item);
        Ok(())
    })
    .await
}

async fn await_registration_with_sink<R: AsyncRead + Unpin>(
    reader: &mut SipStreamReader<R>,
    tracker: &mut RegisterTracker,
    deadline: Instant,
    mut stop: watch::Receiver<bool>,
    mut emit: impl FnMut(SipStreamItem) -> Result<(), RegisterWaitError>,
) -> Result<(RegisterAction, SipResponse), RegisterWaitError> {
    let mut pending = Pending {
        tracker,
        complete: false,
    };
    if !matches!(
        pending.tracker.phase,
        RegisterPhase::AwaitingInitial | RegisterPhase::AwaitingAuthenticated
    ) {
        pending.complete = true;
        return Err(RegisterWaitError::Sip(SipError::InvalidTransition));
    }
    loop {
        if *stop.borrow() || stop.has_changed().is_err() {
            return Err(RegisterWaitError::Cancelled);
        }
        if Instant::now() >= deadline {
            return Err(RegisterWaitError::Deadline);
        }
        let item = tokio::select! {
            biased;
            _=stop.changed()=>continue,
            result=tokio::time::timeout_at(deadline,reader.next())=>result
                .map_err(|_|RegisterWaitError::Deadline)?
                .map_err(RegisterWaitError::Io)?
                .ok_or(RegisterWaitError::Disconnected)?,
        };
        if let SipStreamItem::Message(frame) = &item
            && frame.starts_with(b"SIP/2.0 ")
        {
            let response = SipResponse::parse(frame).map_err(RegisterWaitError::Sip)?;
            let matches = response.register_transaction().is_ok_and(|transaction| {
                transaction.call_id == pending.tracker.call_id
                    && transaction.cseq == pending.tracker.cseq
            });
            if matches {
                let action = pending
                    .tracker
                    .accept(&response)
                    .map_err(RegisterWaitError::Sip)?;
                if action == RegisterAction::Wait {
                    continue;
                }
                pending.complete = true;
                return Ok((action, response));
            }
        }
        emit(item)?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SecAgreeMode;
    use std::time::Duration;
    use tokio::io::AsyncWriteExt;

    fn response(status: u16, call: &str, cseq: u32, extra: &str) -> Vec<u8> {
        format!("SIP/2.0 {status} Test\r\nCall-ID: {call}\r\nCSeq: {cseq} REGISTER\r\n{extra}Content-Length: 0\r\n\r\n").into_bytes()
    }

    #[tokio::test]
    async fn challenge_registration_preserves_incoming_call_and_other_transaction() {
        let (mut writer, input) = tokio::io::duplex(2048);
        let mut reader = SipStreamReader::new(input);
        let mut tracker = RegisterTracker::begin("own", 1, SecAgreeMode::Disabled).unwrap();
        let (_stop, stop) = watch::channel(false);
        let (events, mut incoming) = mpsc::channel(4);
        let invite = b"INVITE sip:user@ims SIP/2.0\r\nContent-Length: 0\r\n\r\n";
        writer.write_all(invite).await.unwrap();
        writer
            .write_all(&response(200, "other", 1, ""))
            .await
            .unwrap();
        writer
            .write_all(&response(100, "own", 1, ""))
            .await
            .unwrap();
        writer
            .write_all(&response(
                401,
                "own",
                1,
                "WWW-Authenticate: Digest realm=\"ims\",nonce=\"x\",algorithm=AKAv1-MD5\r\n",
            ))
            .await
            .unwrap();
        let (action, _) = await_registration(
            &mut reader,
            &mut tracker,
            Instant::now() + Duration::from_secs(1),
            stop.clone(),
            &events,
        )
        .await
        .unwrap();
        assert_eq!(
            action,
            RegisterAction::AnswerAka {
                install_ipsec: false
            }
        );
        let SipStreamItem::Message(frame) = incoming.recv().await.unwrap() else {
            panic!("lost INVITE")
        };
        assert_eq!(&*frame, invite);
        assert!(matches!(
            incoming.recv().await,
            Some(SipStreamItem::Message(_))
        ));
        tracker.authenticated_request_sent(2).unwrap();
        writer
            .write_all(&response(200, "own", 2, ""))
            .await
            .unwrap();
        let (action, _) = await_registration(
            &mut reader,
            &mut tracker,
            Instant::now() + Duration::from_secs(1),
            stop,
            &events,
        )
        .await
        .unwrap();
        assert_eq!(action, RegisterAction::Registered { ipsec: false });
    }

    #[tokio::test]
    async fn retained_events_keep_fifo_without_an_external_consumer() {
        let (mut writer, input) = tokio::io::duplex(2048);
        let mut reader = SipStreamReader::new(input);
        let mut tracker = RegisterTracker::begin("own", 1, SecAgreeMode::Disabled).unwrap();
        let (_stop, stop) = watch::channel(false);
        let mut retained = std::collections::VecDeque::new();
        let invite=b"INVITE sip:user@ims SIP/2.0\r\nCall-ID: early-invite\r\nCSeq: 1 INVITE\r\nContent-Length: 0\r\n\r\n";
        writer.write_all(invite).await.unwrap();
        writer.write_all(b"\r\n").await.unwrap();
        writer
            .write_all(&response(200, "own", 1, ""))
            .await
            .unwrap();
        assert_eq!(
            await_registration_retained(
                &mut reader,
                &mut tracker,
                Instant::now() + Duration::from_secs(1),
                stop,
                &mut retained
            )
            .await
            .unwrap()
            .0,
            RegisterAction::Registered { ipsec: false }
        );
        let Some(SipStreamItem::Message(frame)) = retained.pop_front() else {
            panic!("early INVITE lost")
        };
        assert_eq!(&*frame, invite);
        assert!(matches!(
            retained.pop_front(),
            Some(SipStreamItem::KeepAliveLine)
        ));
        assert!(retained.is_empty());
    }

    #[tokio::test]
    async fn retained_event_count_and_total_wire_bytes_fail_closed() {
        for large in [false, true] {
            let (mut writer, input) = tokio::io::duplex(4096);
            let mut reader = SipStreamReader::new(input);
            let mut tracker = RegisterTracker::begin("own", 1, SecAgreeMode::Disabled).unwrap();
            let (_stop, stop) = watch::channel(false);
            let mut retained = std::collections::VecDeque::new();
            let writing = async {
                if large {
                    let mut frame =
                        b"MESSAGE sip:user@ims SIP/2.0\r\nContent-Length: 64000\r\n\r\n".to_vec();
                    frame.resize(frame.len() + 64000, b'x');
                    for _ in 0..5 {
                        writer.write_all(&frame).await.unwrap();
                    }
                } else {
                    writer.write_all(&b"\r\n".repeat(33)).await.unwrap();
                }
            };
            let waiting = await_registration_retained(
                &mut reader,
                &mut tracker,
                Instant::now() + Duration::from_secs(1),
                stop,
                &mut retained,
            );
            let ((), result) = tokio::join!(writing, waiting);
            assert!(matches!(
                result,
                Err(RegisterWaitError::EventConsumerUnavailable)
            ));
            assert_eq!(tracker.phase(), RegisterPhase::Failed);
            assert_eq!(retained.len(), if large { 4 } else { 32 });
        }
    }

    #[tokio::test]
    async fn provisional_traffic_does_not_extend_deadline() {
        let (mut writer, input) = tokio::io::duplex(512);
        let mut reader = SipStreamReader::new(input);
        let mut tracker = RegisterTracker::begin("own", 1, SecAgreeMode::Disabled).unwrap();
        let (_stop, stop) = watch::channel(false);
        let (events, _incoming) = mpsc::channel(1);
        let flood = async {
            loop {
                writer
                    .write_all(&response(100, "own", 1, ""))
                    .await
                    .unwrap();
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        };
        let result = tokio::select! {
            result=await_registration(&mut reader,&mut tracker,Instant::now()+Duration::from_millis(15),stop,&events)=>result,
            _=flood=>panic!("provisional sender unexpectedly ended"),
        };
        assert!(matches!(result, Err(RegisterWaitError::Deadline)));
        assert_eq!(tracker.phase(), RegisterPhase::Failed);
    }

    #[tokio::test]
    async fn caller_timeout_and_eof_invalidate_registration() {
        let (_writer, input) = tokio::io::duplex(64);
        let mut reader = SipStreamReader::new(input);
        let mut tracker = RegisterTracker::begin("own", 1, SecAgreeMode::Disabled).unwrap();
        let (_stop, stop) = watch::channel(false);
        let (events, _incoming) = mpsc::channel(1);
        assert!(
            tokio::time::timeout(
                Duration::from_millis(10),
                await_registration(
                    &mut reader,
                    &mut tracker,
                    Instant::now() + Duration::from_secs(10),
                    stop.clone(),
                    &events
                )
            )
            .await
            .is_err()
        );
        assert_eq!(tracker.phase(), RegisterPhase::Failed);
        let mut reader = SipStreamReader::new(&b""[..]);
        let mut tracker = RegisterTracker::begin("own", 1, SecAgreeMode::Disabled).unwrap();
        assert!(matches!(
            await_registration(
                &mut reader,
                &mut tracker,
                Instant::now() + Duration::from_secs(1),
                stop,
                &events
            )
            .await,
            Err(RegisterWaitError::Disconnected)
        ));
        assert_eq!(tracker.phase(), RegisterPhase::Failed);
    }
}
