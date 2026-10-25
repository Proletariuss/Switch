//! 9eSIM B0 pre-download BF71 authentication.
//!
//! This card-download authorization is separate from the B1 NBLE Bluetooth
//! password handshake. PSKs are fetched only for the observed EID range and
//! stored in the macOS login Keychain; neither PSK nor EID is written to logs.

use std::{io::Read, time::Duration};

use aes::{
    Aes128,
    cipher::{Block, BlockCipherEncrypt, KeyInit},
};
use proletarius_lpa::RspCard;
use zeroize::Zeroizing;

const KEYCHAIN_SERVICE: &str = "clarivate.nine-esim";
const EID_START: &str = "89086029202200002122000030900000";
const EID_END: &str = "89086029202200002122000040000000";
const MAX_AUTH_RESPONSE_BYTES: usize = 8192;

pub(crate) fn supports(eid: &str) -> bool {
    eid.len() == 32
        && eid.bytes().all(|byte| byte.is_ascii_digit())
        && eid >= EID_START
        && eid < EID_END
}

pub(crate) fn authenticate<C: RspCard>(
    card: &mut C,
    eid: &str,
    challenge: &[u8],
) -> Result<(), String> {
    if !supports(eid) {
        return Ok(());
    }
    if challenge.len() != 16 {
        return Err("B0 eUICC challenge has an unsupported length".into());
    }
    let psk = load_psk(eid)?;
    let nonce = Zeroizing::new(random_nonce()?);
    authenticate_with_material(card, &psk, challenge, &nonce[..])
}

// Keep the actual BF71 exchange independently testable with synthetic key
// material and a fake transport, without Keychain, network, or card access.
fn authenticate_with_material<C: RspCard>(
    card: &mut C,
    psk: &[u8],
    challenge: &[u8],
    nonce: &[u8],
) -> Result<(), String> {
    let body = Zeroizing::new(build_request(psk, challenge, nonce)?);
    let response = card
        .transmit_b0_authentication(&body[..])
        .map_err(|_| "B0 BF71 card command failed".to_owned())?;
    if response.len() < 2 {
        return Err("B0 BF71 authentication response is incomplete".into());
    }
    let status = u16::from_be_bytes([response[response.len() - 2], response[response.len() - 1]]);
    if status != 0x9000 {
        return Err(format!("B0 BF71 authentication rejected (SW {status:04X})"));
    }
    Ok(())
}

fn build_request(psk: &[u8], challenge: &[u8], nonce: &[u8]) -> Result<[u8; 27], String> {
    if psk.len() != 16 || challenge.len() != 16 || nonce.len() != 16 {
        return Err("B0 authentication requires 16-byte inputs".into());
    }
    let mut seed = Zeroizing::new([0u8; 16]);
    seed[..8].copy_from_slice(&challenge[..8]);
    seed[8..].copy_from_slice(&nonce[..8]);
    let derived = Zeroizing::new(aes_encrypt(psk, &*seed)?);
    let encrypted = Zeroizing::new(aes_encrypt(&*derived, challenge)?);
    let mut request = [0u8; 27];
    request[..5].copy_from_slice(&[0xbf, 0x71, 0x18, 0x80, 0x04]);
    for index in 0..4 {
        request[5 + index] = encrypted[index] ^ encrypted[index + 8];
    }
    request[9..11].copy_from_slice(&[0x81, 0x10]);
    request[11..].copy_from_slice(nonce);
    Ok(request)
}

fn aes_encrypt(key: &[u8], input: &[u8]) -> Result<[u8; 16], String> {
    let cipher =
        Aes128::new_from_slice(key).map_err(|_| "B0 PSK has an invalid size".to_owned())?;
    let mut block = Zeroizing::new(Block::<Aes128>::default());
    block.copy_from_slice(input);
    cipher.encrypt_block(&mut block);
    let mut output = [0u8; 16];
    output.copy_from_slice(&block);
    Ok(output)
}

fn random_nonce() -> Result<[u8; 16], String> {
    let mut nonce = [0u8; 16];
    getrandom::fill(&mut nonce).map_err(|_| "B0 nonce generation failed".to_owned())?;
    Ok(nonce)
}

fn load_psk(eid: &str) -> Result<Zeroizing<Vec<u8>>, String> {
    let account = format!("b0-psk:{eid}");
    if let Ok(cached) =
        security_framework::passwords::get_generic_password(KEYCHAIN_SERVICE, &account)
        && cached.len() == 16
    {
        return Ok(Zeroizing::new(cached));
    }

    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| "B0 authorization client could not be initialized".to_owned())?;
    let mut url = url::Url::parse("https://key.9esim.com/psk")
        .map_err(|_| "B0 authorization endpoint is invalid".to_owned())?;
    url.query_pairs_mut().append_pair("eid", eid);
    let response = client
        .get(url)
        .send()
        .map_err(|_| "B0 authorization service is unavailable".to_owned())?;
    if !response.status().is_success() {
        return Err(format!(
            "B0 authorization service returned HTTP {}",
            response.status().as_u16()
        ));
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_AUTH_RESPONSE_BYTES as u64)
    {
        return Err("B0 authorization response exceeded its size limit".into());
    }
    let mut bytes = Vec::with_capacity(256);
    response
        .take((MAX_AUTH_RESPONSE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| "B0 authorization response could not be read".to_owned())?;
    if bytes.len() > MAX_AUTH_RESPONSE_BYTES {
        return Err("B0 authorization response exceeded its size limit".into());
    }
    let bytes = Zeroizing::new(bytes);
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct PskResponse {
        psk: Zeroizing<String>,
    }
    let payload: PskResponse = serde_json::from_slice(&bytes[..])
        .map_err(|_| "B0 authorization response was malformed".to_owned())?;
    let hex = payload.psk.as_str();
    if hex.len() != 32 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("B0 authorization service returned an invalid PSK".into());
    }
    let mut psk = Zeroizing::new(Vec::with_capacity(16));
    for pair in hex.as_bytes().chunks_exact(2) {
        let high = hex_nibble(pair[0]).ok_or_else(|| "B0 PSK encoding was invalid".to_owned())?;
        let low = hex_nibble(pair[1]).ok_or_else(|| "B0 PSK encoding was invalid".to_owned())?;
        psk.push((high << 4) | low);
    }
    security_framework::passwords::set_generic_password(KEYCHAIN_SERVICE, &account, &psk)
        .map_err(|_| "B0 PSK could not be secured in the macOS Keychain".to_owned())?;
    Ok(psk)
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{authenticate_with_material, build_request, supports};
    use proletarius_euicc::{CardTransport, DEFAULT_MSS, Euicc};

    struct FakeTransport {
        commands: Vec<Vec<u8>>,
        response: Result<Vec<u8>, &'static str>,
    }

    impl CardTransport for FakeTransport {
        type Error = &'static str;

        fn transmit(&mut self, command: &[u8], response: &mut Vec<u8>) -> Result<(), Self::Error> {
            self.commands.push(command.to_vec());
            response.clear();
            match &self.response {
                Ok(bytes) => {
                    response.extend_from_slice(bytes);
                    Ok(())
                }
                Err(error) => Err(*error),
            }
        }
    }

    fn fake_card(response: Result<Vec<u8>, &'static str>) -> Euicc<FakeTransport> {
        Euicc::new(
            FakeTransport {
                commands: Vec::new(),
                response,
            },
            1,
            DEFAULT_MSS,
        )
        .unwrap()
    }

    #[test]
    fn b0_range_is_strict_and_does_not_match_other_cards() {
        assert!(supports("89086029202200002122000030900000"));
        assert!(supports("89086029202200002122000039999999"));
        assert!(!supports("89086029202200002122000040000000"));
        assert!(!supports("89086030000000000000000000000000"));
        assert!(!supports("8908602920220000212200003090000x"));
    }

    #[test]
    fn bf71_request_matches_the_independent_openssl_vector() {
        let psk: Vec<u8> = (0..16).collect();
        let challenge: Vec<u8> = (16..32).collect();
        let nonce: Vec<u8> = (32..48).collect();
        assert_eq!(
            build_request(&psk, &challenge, &nonce).unwrap(),
            [
                0xbf, 0x71, 0x18, 0x80, 0x04, 0x06, 0xa1, 0x8b, 0x06, 0x81, 0x10, 0x20, 0x21, 0x22,
                0x23, 0x24, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2a, 0x2b, 0x2c, 0x2d, 0x2e, 0x2f,
            ]
        );
    }

    #[test]
    fn malformed_inputs_are_rejected_before_apdu() {
        assert!(build_request(&[0; 15], &[0; 16], &[0; 16]).is_err());
        assert!(build_request(&[0; 16], &[0; 15], &[0; 16]).is_err());
        assert!(build_request(&[0; 16], &[0; 16], &[0; 15]).is_err());
    }

    #[test]
    fn successful_authentication_sends_exact_bf71_once_on_selected_channel() {
        let mut card = fake_card(Ok(vec![0x90, 0x00]));
        let psk: Vec<u8> = (0..16).collect();
        let challenge: Vec<u8> = (16..32).collect();
        let nonce: Vec<u8> = (32..48).collect();

        assert!(authenticate_with_material(&mut card, &psk, &challenge, &nonce).is_ok());
        assert_eq!(
            card.into_inner().commands,
            [vec![
                0x81, 0xe2, 0x91, 0x00, 0x1b, 0xbf, 0x71, 0x18, 0x80, 0x04, 0x06, 0xa1, 0x8b, 0x06,
                0x81, 0x10, 0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2a, 0x2b,
                0x2c, 0x2d, 0x2e, 0x2f,
            ]]
        );
    }

    #[test]
    fn rejected_authentication_stops_without_retrying() {
        let mut card = fake_card(Ok(vec![0x69, 0x82]));
        assert_eq!(
            authenticate_with_material(&mut card, &[0; 16], &[1; 16], &[2; 16]),
            Err("B0 BF71 authentication rejected (SW 6982)".into())
        );
        assert_eq!(card.into_inner().commands.len(), 1);
    }

    #[test]
    fn incomplete_authentication_responses_fail_closed_without_retrying() {
        for response in [vec![], vec![0x90]] {
            let mut card = fake_card(Ok(response));
            assert_eq!(
                authenticate_with_material(&mut card, &[0; 16], &[1; 16], &[2; 16]),
                Err("B0 BF71 authentication response is incomplete".into())
            );
            assert_eq!(card.into_inner().commands.len(), 1);
        }
    }

    #[test]
    fn transport_failure_stops_without_retrying_or_exposing_transport_details() {
        let mut card = fake_card(Err("synthetic transport diagnostic"));
        assert_eq!(
            authenticate_with_material(&mut card, &[0; 16], &[1; 16], &[2; 16]),
            Err("B0 BF71 card command failed".into())
        );
        assert_eq!(card.into_inner().commands.len(), 1);
    }

    #[test]
    fn invalid_authentication_material_never_touches_transport() {
        for (key_len, challenge_len, nonce_len) in [(15, 16, 16), (16, 15, 16), (16, 16, 15)] {
            let mut card = fake_card(Ok(vec![0x90, 0x00]));
            assert!(
                authenticate_with_material(
                    &mut card,
                    &vec![0; key_len],
                    &vec![1; challenge_len],
                    &vec![2; nonce_len],
                )
                .is_err()
            );
            assert!(card.into_inner().commands.is_empty());
        }
    }
}
