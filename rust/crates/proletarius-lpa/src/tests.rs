// Copyright (c) 2026 anti-rainer
// SPDX-License-Identifier: MIT

use std::{string::String, time::Duration, vec::Vec};

use proletarius_euicc::{
    CancelSessionReason, InstalledProfile, PendingNotification, PrepareDownloadRequest,
    ServerAuthenticationRequest,
};

use crate::{
    ActivationCode, AuthenticateResponse, BoundProfilePackage, Client, DownloadObserver,
    DownloadStage, Error, Es9Transport, InitiateResponse, RspCard,
};

#[derive(Default)]
struct FakeCard {
    calls: Vec<&'static str>,
    notification_present: bool,
    fail_authenticate_server: bool,
    cancel_reason: Option<CancelSessionReason>,
}

impl RspCard for FakeCard {
    type Error = &'static str;

    fn challenge(&mut self) -> Result<Vec<u8>, Self::Error> {
        self.calls.push("challenge");
        Ok(vec![1])
    }

    fn euicc_info1(&mut self) -> Result<Vec<u8>, Self::Error> {
        self.calls.push("euicc_info1");
        Ok(vec![2])
    }

    fn authenticate_server(
        &mut self,
        _input: ServerAuthenticationRequest<'_>,
    ) -> Result<Vec<u8>, Self::Error> {
        self.calls.push("authenticate_server");
        if self.fail_authenticate_server {
            Err("card authentication failed")
        } else {
            Ok(vec![3])
        }
    }

    fn transmit_b0_authentication(&mut self, _body: &[u8]) -> Result<Vec<u8>, Self::Error> {
        self.calls.push("b0_authenticate");
        Ok(vec![0x90, 0x00])
    }

    fn prepare_download(
        &mut self,
        _input: PrepareDownloadRequest<'_>,
    ) -> Result<Vec<u8>, Self::Error> {
        self.calls.push("prepare_download");
        Ok(vec![4])
    }

    fn install(&mut self, _package: &[u8]) -> Result<InstalledProfile, Self::Error> {
        self.calls.push("install");
        Ok(InstalledProfile {
            transaction_id: vec![0xaa],
            isdp_aid: vec![0xbb],
        })
    }

    fn cancel(
        &mut self,
        _transaction_id: &[u8],
        reason: CancelSessionReason,
    ) -> Result<Vec<u8>, Self::Error> {
        self.calls.push("cancel");
        self.cancel_reason = Some(reason);
        Ok(vec![5])
    }

    fn retrieve_notification(
        &mut self,
        sequence_number: u64,
    ) -> Result<Option<PendingNotification>, Self::Error> {
        self.calls.push("retrieve_notification");
        Ok(self.notification_present.then(|| PendingNotification {
            sequence_number,
            address: String::from("https://smdp.example/notify"),
            encoded: vec![6],
        }))
    }

    fn remove_notification(&mut self, _sequence_number: u64) -> Result<(), Self::Error> {
        self.calls.push("remove_notification");
        self.notification_present = false;
        Ok(())
    }
}

#[derive(Default)]
struct FakeEs9 {
    fail_authenticate_client: bool,
    fail_cancel: bool,
    fail_notification: bool,
    profile_metadata: Option<Vec<u8>>,
}

impl Es9Transport for FakeEs9 {
    type Error = &'static str;

    fn initiate(
        &self,
        _activation: &ActivationCode,
        _challenge: &[u8],
        _euicc_info1: &[u8],
        _timeout: Duration,
    ) -> Result<InitiateResponse, Self::Error> {
        Ok(InitiateResponse {
            server_transaction_id: String::from("aabb"),
            transaction_id: vec![0xaa, 0xbb],
            signed1: vec![1],
            signature1: vec![2],
            euicc_ci_pk_id: vec![3],
            server_certificate: vec![4],
        })
    }

    fn authenticate_client(
        &self,
        _activation: &ActivationCode,
        _server_transaction_id: &str,
        _authenticate_server_response: &[u8],
        _timeout: Duration,
    ) -> Result<AuthenticateResponse, Self::Error> {
        if self.fail_authenticate_client {
            return Err("client authentication failed");
        }
        Ok(AuthenticateResponse {
            server_transaction_id: String::from("aabb"),
            transaction_id: vec![0xaa, 0xbb],
            profile_metadata: self
                .profile_metadata
                .clone()
                .unwrap_or_else(|| vec![0xE3, 0x00]),
            signed2: vec![0xa0, 0x00],
            signature2: vec![8],
            server_certificate: vec![7],
        })
    }

    fn get_bound_profile_package(
        &self,
        _activation: &ActivationCode,
        _server_transaction_id: &str,
        _prepare_download_response: &[u8],
        _timeout: Duration,
    ) -> Result<BoundProfilePackage, Self::Error> {
        Ok(BoundProfilePackage {
            server_transaction_id: String::from("aabb"),
            transaction_id: vec![0xaa, 0xbb],
            package: vec![0xde, 0xad],
        })
    }

    fn handle_notification(
        &self,
        _address: &str,
        _notification: &[u8],
        _timeout: Duration,
    ) -> Result<(), Self::Error> {
        if self.fail_notification {
            Err("notification failed")
        } else {
            Ok(())
        }
    }

    fn cancel_session(
        &self,
        _activation: &ActivationCode,
        _server_transaction_id: &str,
        _cancel_response: &[u8],
        _timeout: Duration,
    ) -> Result<(), Self::Error> {
        if self.fail_cancel {
            Err("cancel failed")
        } else {
            Ok(())
        }
    }
}

#[derive(Default)]
struct Observer {
    stages: Vec<DownloadStage>,
    warnings: Vec<String>,
    confirm: bool,
    cancelled: bool,
    metadata: Option<crate::ProfileMetadata>,
    estimated_size: Option<i64>,
}

impl DownloadObserver for Observer {
    fn on_stage(&mut self, stage: DownloadStage) {
        self.stages.push(stage);
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled
    }

    fn confirm_profile(&mut self, _profile_metadata: &[u8]) -> bool {
        self.confirm
    }

    fn confirm_profile_details(
        &mut self,
        _encoded_metadata: &[u8],
        metadata: &crate::ProfileMetadata,
        estimated_profile_size_bytes: i64,
    ) -> bool {
        self.metadata = Some(metadata.clone());
        self.estimated_size = Some(estimated_profile_size_bytes);
        self.confirm
    }

    fn on_warning(&mut self, message: &str) {
        self.warnings.push(String::from(message));
    }
}

fn activation() -> ActivationCode {
    ActivationCode::parse("LPA:1$smdp.example$matching-id").unwrap()
}

#[test]
fn activation_code_parsing_bounds_server_and_keeps_confirmation_code_out_of_display() {
    let mut activation = ActivationCode::parse("LPA:1$smdp.example$matching-id$$1").unwrap();
    activation
        .set_confirmation_code(Some("local-secret"))
        .unwrap();
    assert_eq!(activation.server_address(), "smdp.example");
    assert_eq!(activation.matching_id(), "matching-id");
    assert!(activation.confirmation_code_required());
    assert_eq!(activation.confirmation_code(), Some("local-secret"));
    assert!(!activation.to_string().contains("local-secret"));

    for invalid in [
        "LPA:2$smdp.example$id",
        "LPA:1$https://smdp.example$id",
        "LPA:1$smdp.example/path$id",
        "LPA:1$user@smdp.example$id",
    ] {
        assert!(ActivationCode::parse(invalid).is_err(), "{invalid}");
    }
}

#[test]
fn download_runs_card_and_es9_steps_in_order() {
    let card = FakeCard::default();
    let network = FakeEs9::default();
    let mut client = Client::new(card, network);
    let mut observer = Observer {
        confirm: true,
        ..Observer::default()
    };

    let installed = client
        .download(&activation(), "123456789012345", &mut observer)
        .unwrap();
    assert_eq!(installed.transaction_id, [0xaa]);
    assert_eq!(installed.isdp_aid, [0xbb]);
    assert_eq!(
        observer.stages,
        [
            DownloadStage::InitiateAuthentication,
            DownloadStage::AuthenticateEuicc,
            DownloadStage::AuthenticateClient,
            DownloadStage::AuthenticateServer,
            DownloadStage::Install,
            DownloadStage::Done
        ]
    );
    let (card, _) = client.into_parts();
    assert_eq!(
        card.calls,
        [
            "challenge",
            "euicc_info1",
            "authenticate_server",
            "prepare_download",
            "install"
        ]
    );
}

#[test]
fn pre_authentication_hook_runs_before_authenticate_server() {
    let mut client = Client::new(FakeCard::default(), FakeEs9::default());
    let mut observer = Observer {
        confirm: true,
        ..Observer::default()
    };
    client
        .download_with_eid_and_auth_hook(
            &activation(),
            "123456789012345",
            "fixture-eid",
            &mut observer,
            |card, eid, challenge| {
                assert_eq!(eid, "fixture-eid");
                assert_eq!(challenge, [1]);
                let response = card.transmit_b0_authentication(&[0; 24])?;
                assert_eq!(response, [0x90, 0x00]);
                Ok(())
            },
        )
        .unwrap();
    let (card, _) = client.into_parts();
    assert_eq!(
        card.calls
            .iter()
            .position(|call| *call == "b0_authenticate"),
        Some(2)
    );
    assert_eq!(
        card.calls
            .iter()
            .position(|call| *call == "authenticate_server"),
        Some(3)
    );
}

#[test]
fn pre_authentication_failure_stops_before_authenticate_server() {
    let mut client = Client::new(FakeCard::default(), FakeEs9::default());
    let mut observer = Observer::default();
    assert!(matches!(
        client.download_with_eid_and_auth_hook(
            &activation(),
            "123456789012345",
            "fixture-eid",
            &mut observer,
            |_, _, _| Err("BF71 rejected".into()),
        ),
        Err(Error::Authentication(reason)) if reason == "BF71 rejected"
    ));
    let (card, _) = client.into_parts();
    assert!(!card.calls.contains(&"authenticate_server"));
    assert!(card.calls.contains(&"cancel"));
}

#[test]
fn rejected_profile_cancels_the_card_and_server_session() {
    let mut client = Client::new(FakeCard::default(), FakeEs9::default());
    let mut observer = Observer::default();
    assert!(matches!(
        client.download(&activation(), "123456789012345", &mut observer),
        Err(Error::Rejected)
    ));
    let (card, _) = client.into_parts();
    assert_eq!(card.calls.last(), Some(&"cancel"));
    assert_eq!(card.cancel_reason, Some(CancelSessionReason::Postponed));
}

#[test]
fn download_confirmation_receives_parsed_profile_and_eum_reference_size() {
    let metadata = vec![
        0xE3, 0x10, 0x91, 0x07, b'C', b'O', b'S', b'M', b'O', b'T', b'E', 0xB7, 0x05, 0x80, 0x03,
        0x02, 0xF2, 0x10,
    ];
    let network = FakeEs9 {
        profile_metadata: Some(metadata),
        ..FakeEs9::default()
    };
    let mut client = Client::new(FakeCard::default(), network);
    let mut observer = Observer {
        confirm: true,
        ..Observer::default()
    };

    let activation = ActivationCode::parse("LPA:1$rsp-0003.oberthur.net$matching-id").unwrap();
    client
        .download_with_eid(
            &activation,
            "123456789012345",
            "89044045930000000000001714825038",
            &mut observer,
        )
        .unwrap();

    let metadata = observer.metadata.unwrap();
    assert_eq!(metadata.service_provider_name.as_deref(), Some("COSMOTE"));
    assert_eq!(metadata.plmn.as_deref(), Some("20201"));
    assert_eq!(metadata.estimated_profile_size_bytes, None);
    assert_eq!(observer.estimated_size, Some(39_008));
}

#[test]
fn malformed_profile_metadata_is_cancelled_before_confirmation_or_install() {
    let network = FakeEs9 {
        profile_metadata: Some(vec![0xE3, 0x03, 0x91]),
        ..FakeEs9::default()
    };
    let mut client = Client::new(FakeCard::default(), network);
    let mut observer = Observer {
        confirm: true,
        ..Observer::default()
    };

    assert!(matches!(
        client.download(&activation(), "123456789012345", &mut observer),
        Err(Error::Invalid("invalid profile metadata"))
    ));
    assert!(observer.metadata.is_none());
    let (card, _) = client.into_parts();
    assert_eq!(card.calls.last(), Some(&"cancel"));
    assert_eq!(
        card.cancel_reason,
        Some(CancelSessionReason::MetadataMismatch)
    );
    assert!(!card.calls.contains(&"prepare_download"));
    assert!(!card.calls.contains(&"install"));
}

#[test]
fn network_failure_preserves_primary_error_and_reports_cancel_failure() {
    let mut network = FakeEs9::default();
    network.fail_authenticate_client = true;
    network.fail_cancel = true;
    let mut client = Client::new(FakeCard::default(), network);
    let mut observer = Observer {
        confirm: true,
        ..Observer::default()
    };
    assert!(matches!(
        client.download(&activation(), "123456789012345", &mut observer),
        Err(Error::Network("client authentication failed"))
    ));
    assert_eq!(observer.warnings.len(), 1);
    assert!(observer.warnings[0].contains("cancellation also failed"));
}

#[test]
fn notification_is_removed_only_after_es9_success() {
    let card = FakeCard {
        notification_present: true,
        ..FakeCard::default()
    };
    let mut client = Client::new(card, FakeEs9::default());
    client.retry_notification(7).unwrap();
    let (card, _) = client.into_parts();
    assert!(!card.notification_present);

    let card = FakeCard {
        notification_present: true,
        ..FakeCard::default()
    };
    let network = FakeEs9 {
        fail_notification: true,
        ..FakeEs9::default()
    };
    let mut client = Client::new(card, network);
    assert!(client.retry_notification(7).is_err());
    let (card, _) = client.into_parts();
    assert!(card.notification_present);
}
