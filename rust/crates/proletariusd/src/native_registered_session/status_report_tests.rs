//! Exercise the production registered dispatcher and actor over its real
//! fixture SIP stream, including persistence failure and stale generations.
use super::*;
use crate::ims_sms_transactions::NativeSmsArchive;
use proletarius_store::ArchiveStore;

const REPORT_FILE: &str = "sms-delivery-reports.json";

fn status_request(call_id: &str, cseq: u32, reference: u8) -> Request {
    let tpdu = [
        2, 1, 4, 0x91, 0x44, 0x77, 0x62, 0x80, 0x62, 0x61, 0x21, 0, 0, 0x62, 0x80, 0x62, 0x61,
        0x21, 0, 0, 0,
    ];
    let mut rpdu = vec![1, reference, 0, 0, tpdu.len() as u8];
    rpdu.extend_from_slice(&tpdu);
    let mut headers = Headers::default();
    for (name, value) in [
        ("Via", "SIP/2.0/TCP network;branch=z9hG4bKstatus"),
        ("From", "<sip:smsc@ims.example>"),
        ("P-Asserted-Identity", "<sip:smsc@ims.example>"),
        ("To", "<sip:authorized@ims.example>"),
        ("Content-Type", "application/vnd.3gpp.sms"),
    ] {
        headers.push(Header::new(name, value).unwrap());
    }
    headers.push(Header::new("Call-ID", call_id).unwrap());
    headers.push(Header::new("CSeq", format!("{cseq} MESSAGE")).unwrap());
    Request {
        method: Method::MESSAGE,
        uri: Uri::parse("sip:authorized@ims.example").unwrap(),
        data: MessageData::new("SIP/2.0", headers, rpdu),
    }
}

fn injected_request(generation: SessionGeneration, request: Request) -> Event {
    event(
        generation,
        RegisteredSipEvent::Message {
            connection: SipConnectionId::Primary,
            response_ticket: None,
            message: Box::new(RegisteredSipMessage::Request {
                method: request.method,
                uri: request.uri,
                version: request.data.version,
                headers: request.data.headers,
                body: Zeroizing::new(request.data.body),
            }),
        },
    )
}

async fn finish_report(h: &mut Harness, expected_reference: u8) {
    let report = h.request(Method::MESSAGE).await;
    assert_eq!(
        report.data.body,
        proletarius_sms::build_ack(expected_reference)
    );
    let submitted = tokio::time::timeout(BUDGET, h.notices.recv())
        .await
        .unwrap()
        .unwrap();
    let Notice::Sms(SessionOutcome::ReportSubmitted(ticket)) = submitted else {
        panic!("status report must submit a tracked RP-ACK");
    };
    h.peer(Message::Response(response(&report, 200, vec![], None)))
        .await;
    let completed = tokio::time::timeout(BUDGET, h.notices.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(completed, Notice::Sms(SessionOutcome::ReportFinished {
        ticket: finished, state: DeliveryReportState::Accepted, ..
    }) if finished == ticket)
    );
}

#[tokio::test]
async fn status_report_commits_before_ack_and_retransmit_is_not_an_inbox_message() {
    for archive in [NativeSmsArchive::Ordinary, NativeSmsArchive::Module] {
        let expected_device = if matches!(archive, NativeSmsArchive::Module) {
            "dji"
        } else {
            "reader"
        };
        let mut h = Harness::new_with_sms_archive(archive).await;
        let root = h.archive.clone();
        let path = root.join(REPORT_FILE);
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(root.join(format!("{REPORT_FILE}.lock")))
            .unwrap();
        lock.lock().unwrap();
        h.peer(Message::Request(status_request("status-inbound-1", 73, 42)))
            .await;
        assert!(
            tokio::time::timeout(Duration::from_millis(50), h.registered.frames.recv())
                .await
                .is_err(),
            "successful protocol receipt preceded archive commit"
        );
        assert!(!path.exists());
        drop(lock);
        let Message::Response(sip) = h.frame().await else {
            panic!("SIP response");
        };
        assert_eq!(sip.status, 200);
        let records = ArchiveStore::new(&path).load().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0]["device"], expected_device);
        assert_eq!(records[0]["status_code"], 0);
        assert_eq!(records[0]["message_reference"], 1);
        assert!(!root.join("sms-archive.json").exists());
        let bytes = std::fs::read(&path).unwrap();
        finish_report(&mut h, 42).await;

        // A retransmission can arrive with a new SIP identity and RP reference.
        // Its TP status report is persisted once but each delivery is answered.
        h.peer(Message::Request(status_request("status-inbound-2", 74, 43)))
            .await;
        let Message::Response(sip) = h.frame().await else {
            panic!("replay SIP response");
        };
        assert_eq!(sip.status, 200);
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        finish_report(&mut h, 43).await;
        let forwardable = crate::message_archive::forwarding_owned_list_from_store_for_test(
            &crate::message_archive::InboxState::default(),
            ArchiveStore::new(root.join("sms-archive.json")),
        )
        .await
        .unwrap();
        assert!(
            forwardable.is_empty(),
            "delivery reports entered device forwarding"
        );
        let exit = h.finish().await;
        assert!(exit.pending_reports.is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[tokio::test]
async fn status_report_archive_failure_has_no_successful_protocol_receipt() {
    for archive in [NativeSmsArchive::Ordinary, NativeSmsArchive::Module] {
        let mut h = Harness::new_with_sms_archive(archive).await;
        let root = h.archive.clone();
        std::fs::create_dir(root.join(REPORT_FILE)).unwrap();
        h.peer(Message::Request(status_request(
            "status-write-failed",
            73,
            42,
        )))
        .await;
        let notice = tokio::time::timeout(BUDGET, h.notices.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            notice,
            Notice::Failed(Failure {
                code: "sms_archive_unavailable",
                ..
            })
        ));
        assert!(
            tokio::time::timeout(Duration::from_millis(50), h.registered.frames.recv())
                .await
                .is_err()
        );
        assert!(!root.join("sms-archive.json").exists());
        let exit = h.finish().await;
        assert!(exit.pending_reports.is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[tokio::test]
async fn stale_generation_status_report_has_no_archive_or_protocol_io() {
    for archive in [NativeSmsArchive::Ordinary, NativeSmsArchive::Module] {
        let mut h = Harness::new_with_sms_archive(archive).await;
        let root = h.archive.clone();
        h.injected
            .send(injected_request(
                session_generation(h.generation.value() + 1),
                status_request("status-retired", 73, 42),
            ))
            .await
            .unwrap();
        // A current-generation unhandled event is a FIFO dispatcher barrier;
        // unlike a control query it cannot overtake the retired MESSAGE.
        let mut barrier = status_request("status-barrier", 74, 43);
        barrier.method = Method::OPTIONS;
        h.injected
            .send(injected_request(h.generation, barrier))
            .await
            .unwrap();
        let notice = tokio::time::timeout(BUDGET, h.notices.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(notice, Notice::Unhandled(_)));
        assert!(!root.join(REPORT_FILE).exists());
        assert!(!root.join("sms-archive.json").exists());
        assert!(h.registered.frames.try_recv().is_err());
        let exit = h.finish().await;
        assert!(exit.pending_reports.is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }
}
