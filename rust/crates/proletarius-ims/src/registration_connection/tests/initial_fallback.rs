use super::*;

fn response(cseq: u32, status: u16) -> Vec<u8> {
    format!("SIP/2.0 {status} Fixture\r\nCall-ID: own\r\nCSeq: {cseq} REGISTER\r\nContent-Length: 0\r\n\r\n").into_bytes()
}
#[tokio::test]
async fn unmatched_rejection_cannot_authorize_an_initial_variant() {
    tokio::time::timeout(Duration::from_secs(2), async {
        let (client, mut server) = tokio::io::duplex(4096);
        let connection = RegistrationConnection::new(client, "own", 1, SecAgreeMode::Disabled)
            .unwrap()
            .with_retained_events();
        let (_owner, stop) = watch::channel(false);
        let (events, _rx) = mpsc::channel(4);
        let peer = async {
            let mut bytes = vec![0; request(1).len()];
            server.read_exact(&mut bytes).await.unwrap();
            server.write_all(&response(999, 403)).await.unwrap();
            server.write_all(&response(1, 200)).await.unwrap();
        };
        let local = async {
            let progress = connection
                .advance_initial_with_fallback(
                    &request(1),
                    Instant::now() + Duration::from_secs(1),
                    stop,
                    &events,
                    |_, _| panic!("foreign rejection invoked fallback"),
                )
                .await
                .unwrap();
            assert!(matches!(progress, RegistrationProgress::Registered(_, _)));
        };
        tokio::join!(local, peer);
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn authenticated_rejection_never_calls_the_initial_hook() {
    tokio::time::timeout(Duration::from_secs(2), async {
        let (client, mut server) = tokio::io::duplex(4096);
        let mut connection =
            RegistrationConnection::new(client, "own", 1, SecAgreeMode::Disabled).unwrap();
        // This is the state entered by a matched AKA challenge. The real TCP
        // coordinator fixture separately performs the actual challenge/AKA.
        connection.tracker.phase = RegisterPhase::ChallengeReceived;
        let (_owner, stop) = watch::channel(false);
        let (events, _rx) = mpsc::channel(4);
        let peer = async {
            let mut bytes = vec![0; request(2).len()];
            server.read_exact(&mut bytes).await.unwrap();
            server.write_all(&response(2, 403)).await.unwrap();
        };
        let local = async {
            let failure = match connection
                .advance_initial_with_fallback(
                    &request(2),
                    Instant::now() + Duration::from_secs(1),
                    stop,
                    &events,
                    |_, _| panic!("authenticated rejection invoked initial hook"),
                )
                .await
            {
                Err(failure) => failure,
                Ok(_) => panic!("unexpected authentication success"),
            };
            let (error, retired) = failure.into_error_and_retired();
            assert!(matches!(
                error,
                RegisterWaitError::Sip(SipError::RegistrationRejected(403))
            ));
            retired.retire().await.unwrap();
        };
        tokio::join!(local, peer);
    })
    .await
    .unwrap();
}
