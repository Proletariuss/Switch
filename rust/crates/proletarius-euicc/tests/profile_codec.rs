use std::collections::VecDeque;

use proletarius_ccid::{CardTransport, bertlv};
use proletarius_euicc::{Euicc, ProfileInfo, decode_iccid, encode_iccid};

#[derive(Debug)]
enum FakeError {
    Transport,
}

struct FakeCard {
    responses: VecDeque<Vec<u8>>,
    commands: Vec<Vec<u8>>,
}

impl CardTransport for FakeCard {
    type Error = FakeError;

    fn transmit(&mut self, command: &[u8], response: &mut Vec<u8>) -> Result<(), Self::Error> {
        self.commands.push(command.to_vec());
        response.clear();
        response.extend(self.responses.pop_front().ok_or(FakeError::Transport)?);
        Ok(())
    }
}

fn response_with_profile(iccid: &[u8]) -> Vec<u8> {
    let mut fields = Vec::new();
    bertlv::encode(&[0x5a], iccid, &mut fields).unwrap();
    let mut profile = Vec::new();
    bertlv::encode_constructed(&[0xe3], &fields, &mut profile).unwrap();
    let mut profiles = Vec::new();
    bertlv::encode_constructed(&[0xa0], &profile, &mut profiles).unwrap();
    let mut outer = Vec::new();
    bertlv::encode_constructed(&[0xbf, 0x2d], &profiles, &mut outer).unwrap();
    outer.extend_from_slice(&[0x90, 0x00]);
    outer
}

#[test]
fn iccid_bcd_round_trip_uses_the_card_representation() {
    let digits = "8901000000000000000";
    let encoded = encode_iccid(digits).unwrap();
    assert_eq!(decode_iccid(&encoded).unwrap(), digits);
}

#[test]
fn profile_snapshot_decodes_iccid_from_es10c_response() {
    let digits = "8901000000000000000";
    let response = response_with_profile(&encode_iccid(digits).unwrap());
    let card = FakeCard {
        responses: VecDeque::from([response]),
        commands: Vec::new(),
    };
    let mut euicc = Euicc::with_default_mss(card, 0).unwrap();
    let mut profiles = vec![ProfileInfo::default(); 1];

    assert_eq!(euicc.list_profiles_into(&mut profiles).unwrap(), 1);
    assert_eq!(profiles[0].iccid_string().unwrap(), digits);
    assert_eq!(euicc.into_inner().commands.len(), 1);
}

#[test]
fn malformed_profile_does_not_partially_replace_existing_snapshot() {
    let response = response_with_profile(&[0x89; 17]);
    let card = FakeCard {
        responses: VecDeque::from([response]),
        commands: Vec::new(),
    };
    let mut euicc = Euicc::with_default_mss(card, 0).unwrap();
    let mut existing = ProfileInfo::default();
    existing.nickname = b"existing snapshot".to_vec();
    let mut profiles = vec![existing];

    assert!(euicc.list_profiles_into(&mut profiles).is_err());
    assert_eq!(profiles[0].nickname, b"existing snapshot");
}
