use zeroize::Zeroizing;

// Keep quoted commas inside their value. Bound allocation before parsing and
// reject ambiguity rather than selecting the first/last duplicate parameter.
pub(crate) fn parse(value: &str) -> Option<Vec<(String, Zeroizing<String>)>> {
    if value.len() > 8192 || value.bytes().any(|b| b.is_ascii_control() && b != b'\t') {
        return None;
    }
    let (scheme, mut rest) = value.trim().split_once(char::is_whitespace)?;
    if !scheme.eq_ignore_ascii_case("Digest") {
        return None;
    }
    let mut result: Vec<(String, Zeroizing<String>)> = Vec::new();
    loop {
        rest = rest.trim_start_matches([' ', '\t']);
        let end = rest.bytes().position(|b| !super::is_header_name_byte(b))?;
        if end == 0 || result.len() >= 32 {
            return None;
        }
        let name = rest[..end].to_ascii_lowercase();
        if result.iter().any(|(existing, _)| existing == &name) {
            return None;
        }
        rest = rest[end..]
            .trim_start_matches([' ', '\t'])
            .strip_prefix('=')?
            .trim_start_matches([' ', '\t']);
        let mut decoded = Zeroizing::new(String::new());
        if let Some(quoted) = rest.strip_prefix('"') {
            let mut chars = quoted.char_indices();
            let consumed = loop {
                let (index, ch) = chars.next()?;
                match ch {
                    '"' => break index + 1,
                    '\\' => decoded.push(chars.next()?.1),
                    _ => decoded.push(ch),
                }
            };
            rest = &quoted[consumed..];
        } else {
            let end = rest
                .bytes()
                .position(|b| !super::is_header_name_byte(b))
                .unwrap_or(rest.len());
            if end == 0 {
                return None;
            }
            decoded.push_str(&rest[..end]);
            rest = &rest[end..];
        }
        result.push((name, decoded));
        rest = rest.trim_start_matches([' ', '\t']);
        if rest.is_empty() {
            return Some(result);
        }
        rest = rest.strip_prefix(',')?;
        if rest.trim().is_empty() {
            return None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoted_extensions_cannot_inject_aka_parameters() {
        assert!(!crate::is_aka_digest_challenge(
            r#"Digest realm="example,algorithm=AKAv1-MD5",nonce="x""#
        ));
        assert!(crate::is_aka_digest_challenge(
            r#"Digest realm="example,one",nonce="x",algorithm=AKAv1-MD5,qop="auth,auth-int""#
        ));
        let values = parse(r#"Digest realm="a\"b\\c",nonce="x""#).unwrap();
        assert_eq!(values[0].1.as_str(), "a\"b\\c");
    }

    #[test]
    fn ambiguous_and_malformed_challenges_are_rejected() {
        for value in [
            "Digest nonce=x,Nonce=y",
            "Digest nonce=x,",
            "Digest nonce=\"x",
            "Digest nonce=\"x\"junk",
            "Digest nonce=x\r\nalgorithm=AKAv1-MD5",
            "Digest nonce=x,,algorithm=AKAv1-MD5",
            "Basic nonce=x",
        ] {
            assert!(parse(value).is_none(), "{value:?}");
        }
        assert!(parse(&format!("Digest nonce=\"{}\"", "x".repeat(8192))).is_none());
        assert!(crate::is_aka_digest_challenge(
            "Digest nonce=x,algorithm=AKAv2-MD5"
        ));
    }
}
