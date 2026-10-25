//! Go newSAForFlow + standard crypto/aes, crypto/des and crypto/hmac vectors.
//! Synthetic fixed inputs; no real subscriber material or production RNG override.
use super::*;
const VECTORS: &[(&str, &str, &str)] = &[
    (
        "aes-cbc",
        "hmac-sha-1-96",
        "0000002a0000000144444444444444444444444444444444ce22fdfecf5e2dc442ad48764f1e45772b303196e300c44280880b576f2adfeae608589ee24191edbc457af1",
    ),
    (
        "aes-cbc",
        "hmac-md5-96",
        "0000002a0000000144444444444444444444444444444444ce22fdfecf5e2dc442ad48764f1e45772b303196e300c44280880b576f2adfea8ac9b35fa7ed78298322e09a",
    ),
    (
        "des-ede3-cbc",
        "hmac-sha-1-96",
        "0000002a000000014444444444444444c738f735fb80236d26ffdd5a685e2cf6f11148288b3c370b237914dcc8856ad770457043",
    ),
    (
        "des-ede3-cbc",
        "hmac-md5-96",
        "0000002a000000014444444444444444c738f735fb80236d26ffdd5a685e2cf6f11148288b3c370b0d44f2ddf116d44e17b196ee",
    ),
    (
        "null",
        "hmac-sha-1-96",
        "0000002a0000000133333333333333333333333333333333333333330006976903ec1e837342d25d93d8",
    ),
    (
        "null",
        "hmac-md5-96",
        "0000002a00000001333333333333333333333333333333333333333300068b7cc0b16d5e20671dc990b1",
    ),
];
fn keys(suite: Suite) -> (Zeroizing<Vec<u8>>, Zeroizing<Vec<u8>>) {
    suite.derive(
        &std::array::from_fn(|i| i as u8),
        &std::array::from_fn(|i| 0x20 + i as u8),
    )
}
fn sender(suite: Suite) -> Sender {
    let (enc, auth) = keys(suite);
    Sender::with_suite(42, suite, enc, auth).unwrap()
}
fn receiver(suite: Suite) -> Receiver {
    let (enc, auth) = keys(suite);
    Receiver::with_suite(42, suite, enc, auth).unwrap()
}
fn unhex(wire: &str) -> Vec<u8> {
    wire.as_bytes()
        .chunks_exact(2)
        .map(|chunk| u8::from_str_radix(std::str::from_utf8(chunk).unwrap(), 16).unwrap())
        .collect()
}
#[test]
fn all_six_suites_match_independent_go_wire_and_key_derivation() {
    for &(enc, auth, wire) in VECTORS {
        let suite = Suite::selected(enc, auth).unwrap();
        let expected = unhex(wire);
        let actual = sender(suite)
            .seal_with_iv(&[0x33; 20], &vec![0x44; suite.iv_len()])
            .unwrap();
        assert_eq!(actual, expected, "{enc}/{auth}");
        assert_eq!(&*receiver(suite).open(&expected).unwrap(), &[0x33; 20]);
        assert_eq!(sealed_len_for_suite(suite, 20), Some(expected.len()));
        let (enc_key, auth_key) = keys(suite);
        assert_eq!(auth_key.len(), suite.integrity_key_len());
        assert_eq!(
            &auth_key[..16],
            &std::array::from_fn::<_, 16, _>(|i| 0x20 + i as u8)
        );
        if suite.integrity == Integrity::Sha1 {
            assert_eq!(&auth_key[16..], &[0; 4]);
        }
        if suite.encryption == Encryption::TripleDesCbc {
            assert_eq!(&enc_key[..8], &enc_key[16..]);
            assert!(enc_key.iter().all(|b| b.count_ones() % 2 == 1));
        }
    }
}
#[test]
fn every_suite_rejects_mac_key_spi_replay_and_does_not_consume_bad_sequences() {
    for &(enc, auth, wire) in VECTORS {
        let suite = Suite::selected(enc, auth).unwrap();
        let valid = unhex(wire);
        let mut rx = receiver(suite);
        for index in [4, 8, valid.len() - 1] {
            let mut bad = valid.clone();
            bad[index] ^= 1;
            assert_eq!(
                rx.open(&bad),
                Err(EspError::Authentication),
                "{enc}/{auth} {index}"
            );
            assert_eq!((rx.highest, rx.window), (0, 0));
        }
        let (enc_key, mut auth_key) = keys(suite);
        auth_key[0] ^= 1;
        assert_eq!(
            Receiver::with_suite(42, suite, enc_key, auth_key)
                .unwrap()
                .open(&valid),
            Err(EspError::Authentication)
        );
        if suite.encryption != Encryption::Null {
            let (mut enc_key, auth_key) = keys(suite);
            enc_key[0] ^= 0x80;
            let mut wrong = Receiver::with_suite(42, suite, enc_key, auth_key).unwrap();
            assert_eq!(wrong.open(&valid), Err(EspError::InvalidPacket));
            assert_eq!(wrong.highest, 0);
        }
        let mut wrong_spi = valid.clone();
        wrong_spi[3] ^= 1;
        assert_eq!(rx.open(&wrong_spi), Err(EspError::InvalidSpi));
        assert_eq!(&*rx.open(&valid).unwrap(), &[0x33; 20]);
        assert_eq!(rx.open(&valid), Err(EspError::Replay));
        let mut tx = sender(suite);
        tx.sequence = 65;
        let far = tx.seal(&[0x33; 20]).unwrap();
        assert!(rx.open(&far).is_ok());
        assert_eq!(rx.open(&valid), Err(EspError::Replay));
        let reordered = tx.seal(&[0x33; 20]).unwrap();
        let last = tx.seal(&[0x33; 20]).unwrap();
        assert!(rx.open(&last).is_ok());
        assert!(rx.open(&reordered).is_ok());
        tx.sequence = u32::MAX;
        assert_eq!(tx.seal(&[0x33; 20]), Err(EspError::Exhausted));
    }
}
#[test]
fn authenticated_trailer_and_cipher_boundaries_are_checked_for_every_suite() {
    for &(enc, auth, _) in VECTORS {
        let suite = Suite::selected(enc, auth).unwrap();
        for len in [19, 65001] {
            assert_eq!(
                sender(suite).seal(&vec![0; len]),
                Err(EspError::InvalidPacket)
            );
        }
        for len in [20, 21, 22, 23, 30, 31, 32, 65000] {
            let payload = vec![0x33; len];
            let valid = sender(suite).seal(&payload).unwrap();
            assert_eq!(valid.len(), sealed_len_for_suite(suite, len).unwrap());
            assert_eq!(&*receiver(suite).open(&valid).unwrap(), &payload);
            for cut in [0, 7, valid.len() - 1] {
                assert!(receiver(suite).open(&valid[..cut]).is_err());
            }
        }
        let iv = vec![0x44; suite.iv_len()];
        let (_, auth_key) = keys(suite);
        let (enc_key, _) = keys(suite);
        for corruption in [0, 1, 2] {
            let mut plain = Zeroizing::new(vec![0x33; 20]);
            let pad =
                (suite.block_len() - (plain.len() + 2) % suite.block_len()) % suite.block_len();
            plain.extend((1..=pad).map(|i| i as u8));
            plain.extend_from_slice(&[pad as u8, 6]);
            match corruption {
                0 => *plain.last_mut().unwrap() = 17,
                1 => {
                    let n = plain.len();
                    plain[n - 2] = 255;
                }
                _ => {
                    if pad > 0 {
                        plain[20] = 0;
                    } else {
                        let n = plain.len();
                        plain[n - 2] = 1;
                    }
                }
            }
            encrypt(suite, &enc_key, &iv, &mut plain).unwrap();
            let mut packet = vec![0, 0, 0, 42, 0, 0, 0, 1];
            packet.extend_from_slice(&iv);
            packet.extend_from_slice(&plain);
            append_icv(suite.integrity, &auth_key, &mut packet).unwrap();
            let mut rx = receiver(suite);
            assert_eq!(rx.open(&packet), Err(EspError::InvalidPacket));
            assert_eq!(rx.highest, 0);
        }
        assert_eq!(sealed_len_for_suite(suite, usize::MAX), None);
    }
}
#[test]
fn null_is_authenticated_esp_and_accepts_valid_peer_alignment_padding() {
    for auth in [Integrity::Sha1, Integrity::Md5] {
        let suite = Suite {
            encryption: Encryption::Null,
            integrity: auth,
        };
        let (_, key) = keys(suite);
        let mut packet = vec![0, 0, 0, 42, 0, 0, 0, 1];
        packet.extend_from_slice(&[0x33; 20]);
        packet.extend_from_slice(&[1, 2, 2, 6]);
        append_icv(auth, &key, &mut packet).unwrap();
        assert_eq!(&*receiver(suite).open(&packet).unwrap(), &[0x33; 20]);
    }
}
#[test]
fn invalid_keys_and_unimplemented_transforms_never_become_unauthenticated_esp() {
    for &(enc, auth, _) in VECTORS {
        let suite = Suite::selected(enc, auth).unwrap();
        let (mut enc_key, auth_key) = keys(suite);
        enc_key.push(0);
        assert!(matches!(
            Sender::with_suite(42, suite, enc_key, auth_key),
            Err(EspError::InvalidKey)
        ));
        let (enc_key, mut auth_key) = keys(suite);
        auth_key.pop();
        assert!(matches!(
            Receiver::with_suite(42, suite, enc_key, auth_key),
            Err(EspError::InvalidKey)
        ));
    }
    for (enc, auth) in [
        ("aes-gcm", "hmac-sha-1-96"),
        ("null", "null"),
        ("aes-cbc", "hmac-sha-256-128"),
        ("des-cbc", "hmac-md5-96"),
    ] {
        assert_eq!(
            Suite::selected(enc, auth),
            Err(EspError::UnsupportedTransform)
        );
    }
}
