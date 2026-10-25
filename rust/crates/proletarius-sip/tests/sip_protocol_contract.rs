use std::time::Duration;

use proletarius_sip::{
    ClientEvent, ClientState, ClientTransaction, Dialog, DialogRole, Message, Method, Parser,
    Reliability, ServerEvent, ServerState, ServerTransaction, StreamParser, TimerConfig,
    TransactionAction, TransactionKind, TransactionTimer,
};

#[test]
fn stream_parser_handles_fragmented_messages_keepalives_and_coalesced_frames() {
    let request = b"INVITE sip:bob@example.net SIP/2.0\r\nVia: SIP/2.0/TCP host;branch=z9hG4bK-a\r\nCall-ID: call-a\r\nCSeq: 1 INVITE\r\nContent-Length: 4\r\n\r\nbody";
    let response =
        b"SIP/2.0 180 Ringing\r\nCall-ID: call-a\r\nCSeq: 1 INVITE\r\nContent-Length: 0\r\n\r\n";
    let mut stream = StreamParser::new(proletarius_sip::MAX_MESSAGE_SIZE);

    assert!(stream.push(&request[..27]).unwrap().is_empty());
    let mut burst = request[27..].to_vec();
    burst.extend_from_slice(b"\r\n");
    burst.extend_from_slice(response);
    let messages = stream.push(&burst).unwrap();

    assert_eq!(messages.len(), 2);
    assert!(matches!(&messages[0], Message::Request(r) if r.method == Method::Invite));
    assert!(matches!(&messages[1], Message::Response(r) if r.status == 180));
    assert_eq!(stream.buffered_len(), 0);
}

#[test]
fn parser_rejects_ambiguous_content_length_and_bounds_input() {
    let parser = Parser::default();
    let conflicting =
        b"OPTIONS sip:ims.example.net SIP/2.0\r\nContent-Length: 0\r\nContent-Length: 1\r\n\r\n";
    assert!(parser.parse(conflicting, false).is_err());

    let oversized = vec![b'x'; proletarius_sip::MAX_MESSAGE_SIZE + 1];
    assert_eq!(
        parser.parse(&oversized, false).unwrap_err(),
        proletarius_sip::Error::MessageTooLarge
    );
}

#[test]
fn dialog_uses_route_set_order_and_separate_ack_cseq_rules() {
    let invite = b"INVITE sip:bob@example.net SIP/2.0\r\nFrom: <sip:alice@example.net>;tag=local\r\nTo: <sip:bob@example.net>\r\nCall-ID: call-7\r\nCSeq: 41 INVITE\r\nContact: <sip:alice@192.0.2.1>\r\nContent-Length: 0\r\n\r\n";
    let ok = b"SIP/2.0 200 OK\r\nFrom: <sip:alice@example.net>;tag=local\r\nTo: <sip:bob@example.net>;tag=remote\r\nCall-ID: call-7\r\nCSeq: 41 INVITE\r\nContact: <sip:bob@192.0.2.2>\r\nRecord-Route: <sip:edge-a.example.net;lr>\r\nRecord-Route: <sip:edge-b.example.net;lr>\r\nContent-Length: 0\r\n\r\n";
    let parser = Parser::default();
    let (Message::Request(invite), _) = parser.parse(invite, false).unwrap() else {
        panic!("expected request")
    };
    let (Message::Response(ok), _) = parser.parse(ok, false).unwrap() else {
        panic!("expected response")
    };
    let mut dialog = Dialog::from_initial_exchange(DialogRole::UserAgentClient, &invite, &ok)
        .expect("valid initial exchange creates dialog");

    assert_eq!(dialog.route_set().len(), 2);
    assert_eq!(dialog.route_set()[0].host, "edge-b.example.net");
    assert_eq!(
        dialog.route_plan().unwrap().next_hop.host,
        "edge-b.example.net"
    );
    assert_eq!(
        dialog
            .build_ack("SIP/2.0/TCP local;branch=z9hG4bK-ack")
            .unwrap()
            .data
            .headers
            .get("CSeq")
            .unwrap()
            .value(),
        "41 ACK"
    );
    assert_eq!(
        dialog
            .build_bye("SIP/2.0/TCP local;branch=z9hG4bK-bye", Vec::new())
            .unwrap()
            .data
            .headers
            .get("CSeq")
            .unwrap()
            .value(),
        "42 BYE"
    );
}

#[test]
fn invite_client_and_server_transactions_keep_ack_and_cancel_events_distinct() {
    let timers = TimerConfig::default();
    let mut client =
        ClientTransaction::new(TransactionKind::Invite, Reliability::Unreliable, timers);
    let started = client.start();
    assert!(started.contains(TransactionAction::TransmitRequest));
    assert!(started.has_timer(TransactionTimer::A));
    assert!(started.has_timer(TransactionTimer::B));

    let provisional = client.on_event(ClientEvent::Provisional);
    assert_eq!(provisional.state, ClientState::Proceeding);
    assert!(
        provisional
            .actions
            .contains(TransactionAction::Cancel(TransactionTimer::A))
    );
    assert!(
        provisional
            .actions
            .contains(TransactionAction::DeliverResponse)
    );

    let failure = client.on_event(ClientEvent::Failure);
    assert_eq!(failure.state, ClientState::Completed);
    assert!(failure.actions.contains(TransactionAction::TransmitAck));
    assert!(failure.actions.has_timer(TransactionTimer::D));

    let mut server =
        ServerTransaction::new(TransactionKind::Invite, Reliability::Unreliable, timers);
    let cancelled = server.on_event(ServerEvent::Cancel);
    assert_eq!(cancelled.state, ServerState::Proceeding);
    assert!(cancelled.actions.contains(TransactionAction::DeliverCancel));
    let success = server.on_event(ServerEvent::UserSuccess);
    assert_eq!(success.state, ServerState::Accepted);
    assert!(success.actions.has_timer(TransactionTimer::G));
    assert_eq!(timers.duration_64_t1(), Duration::from_secs(32));
}
