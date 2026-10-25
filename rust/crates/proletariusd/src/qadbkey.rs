use md5::{Digest, Md5};
use zeroize::{Zeroize, Zeroizing};

const SECRET: &[u8] = b"SH_adb_quectel";
const MAGIC: &str = "$1$";
const ALPHABET: &[u8; 64] = b"./0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

pub(crate) fn challenge(response: &str) -> Option<Zeroizing<String>> {
    let mut value: Option<String> = None;
    for line in response.lines().map(str::trim) {
        let marker = "+QADBKEY:";
        if line
            .get(.."+QADBKEY".len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("+QADBKEY"))
        {
            let Some(prefix) = line.get(..marker.len()) else {
                return None;
            };
            if !prefix.eq_ignore_ascii_case(marker) {
                return None;
            }
        } else {
            continue;
        }
        let parsed = line[marker.len()..].trim();
        if parsed.len() != 8 || !parsed.bytes().all(|byte| byte.is_ascii_digit()) || value.is_some()
        {
            return None;
        }
        value = Some(parsed.to_owned());
    }
    value.map(Zeroizing::new)
}

pub(crate) fn password(challenge: &str) -> Option<Zeroizing<String>> {
    if challenge.len() != 8 || !challenge.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let hash = md5_crypt(SECRET, challenge.as_bytes());
    let prefix = format!("{MAGIC}{challenge}$");
    let value = hash.strip_prefix(&prefix)?;
    (value.len() >= 15).then(|| Zeroizing::new(value[..15].to_owned()))
}

fn md5_crypt(password: &[u8], salt: &[u8]) -> Zeroizing<String> {
    let mut initial = Md5::new();
    initial.update(password);
    initial.update(MAGIC.as_bytes());
    initial.update(salt);

    let mut alternate = Md5::new();
    alternate.update(password);
    alternate.update(salt);
    alternate.update(password);
    let alternate_sum = alternate.finalize();
    let alternate_bytes = Zeroizing::new(alternate_sum.to_vec());
    let mut remaining = password.len();
    while remaining > 0 {
        let count = remaining.min(alternate_bytes.len());
        initial.update(&alternate_bytes[..count]);
        remaining -= count;
    }
    remaining = password.len();
    while remaining > 0 {
        if remaining & 1 != 0 {
            initial.update([0]);
        } else {
            initial.update(&password[..1]);
        }
        remaining >>= 1;
    }
    let first = initial.finalize();
    let mut digest = Zeroizing::new(first.to_vec());
    for round in 0..1000 {
        let mut current = Md5::new();
        if round & 1 != 0 {
            current.update(password);
        } else {
            current.update(&digest[..]);
        }
        if round % 3 != 0 {
            current.update(salt);
        }
        if round % 7 != 0 {
            current.update(password);
        }
        if round & 1 != 0 {
            current.update(&digest[..]);
        } else {
            current.update(password);
        }
        let next = current.finalize();
        let mut next_bytes = Zeroizing::new(next.to_vec());
        digest.zeroize();
        std::mem::swap(&mut *digest, &mut *next_bytes);
    }

    let mut encoded = Zeroizing::new(String::with_capacity(22));
    write_base64(&mut encoded, digest[0], digest[6], digest[12], 4);
    write_base64(&mut encoded, digest[1], digest[7], digest[13], 4);
    write_base64(&mut encoded, digest[2], digest[8], digest[14], 4);
    write_base64(&mut encoded, digest[3], digest[9], digest[15], 4);
    write_base64(&mut encoded, digest[4], digest[10], digest[5], 4);
    write_base64(&mut encoded, 0, 0, digest[11], 2);

    Zeroizing::new(format!(
        "{MAGIC}{}${}",
        String::from_utf8_lossy(salt),
        encoded.as_str()
    ))
}

fn write_base64(output: &mut String, high: u8, middle: u8, low: u8, count: usize) {
    let mut value = u32::from(high) << 16 | u32::from(middle) << 8 | u32::from(low);
    for _ in 0..count {
        output.push(ALPHABET[(value & 0x3f) as usize] as char);
        value >>= 6;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_qadbkey_matches_known_vectors() {
        for (challenge, expected) in [
            ("12345678", "0jXKXQwSwMxYoeg"),
            ("15478726", "n9Qq0s1x4LtgAvt"),
            ("31711264", "SV3LHz1ynUZZmYU"),
        ] {
            assert_eq!(
                password(challenge).as_deref().map(String::as_str),
                Some(expected)
            );
        }
        assert!(password("1234").is_none());
        assert!(password("12345678\nAT+CFUN=1,1").is_none());
    }

    #[test]
    fn challenge_parser_requires_exactly_one_eight_digit_value() {
        assert_eq!(
            challenge("AT+QADBKEY?\r\n+QADBKEY: 12345678\r\nOK")
                .as_deref()
                .map(String::as_str),
            Some("12345678")
        );
        for response in [
            "+QADBKEY: 1234567\r\nOK",
            "+QADBKEY\r\nOK",
            "+QADBKEY: 12345678 extra\r\nOK",
            "+QADBKEY: 12345678\r\n+QADBKEY: 87654321\r\nOK",
            "+QADBKEY: 1234567x\r\nOK",
        ] {
            assert!(
                challenge(response).is_none(),
                "accepted malformed reply: {response:?}"
            );
        }
    }
}
