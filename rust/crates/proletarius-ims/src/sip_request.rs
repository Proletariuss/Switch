//! Bounded SIP request wire encoding shared by signaling-only IMS flows.
//! Transport ownership stays with the caller.

use zeroize::Zeroizing;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SipRequestError {
    InvalidMethod,
    InvalidUri,
    InvalidHeader,
    InvalidCSeq,
    MissingHeader,
    MessageTooLarge,
}

pub struct SipRequest<'a> {
    pub method: &'a str,
    pub uri: &'a str,
    pub headers: &'a [(&'a str, &'a str)],
    pub body: &'a [u8],
}

impl SipRequest<'_> {
    pub fn encode(&self) -> Result<Zeroizing<Vec<u8>>, SipRequestError> {
        if !matches!(
            self.method,
            "INVITE" | "ACK" | "CANCEL" | "BYE" | "INFO" | "PRACK"
        ) {
            return Err(SipRequestError::InvalidMethod);
        }
        if !(self.uri.starts_with("sip:")
            || self.uri.starts_with("sips:")
            || self.uri.starts_with("tel:"))
            || self.uri.is_empty()
            || self.uri.len() > 4096
            || self
                .uri
                .bytes()
                .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
        {
            return Err(SipRequestError::InvalidUri);
        }
        if self.headers.len() > 120 || self.body.len() > 65_535 {
            return Err(SipRequestError::MessageTooLarge);
        }

        let mut required = [false; 6];
        let mut out =
            Zeroizing::new(format!("{} {} SIP/2.0\r\n", self.method, self.uri).into_bytes());
        for &(name, value) in self.headers {
            if name.is_empty()
                || !name.bytes().all(crate::is_header_name_byte)
                || value.trim().is_empty()
                || value.bytes().any(|byte| byte.is_ascii_control())
            {
                return Err(SipRequestError::InvalidHeader);
            }
            if name.len() + value.len() + 2 > 8192 {
                return Err(SipRequestError::MessageTooLarge);
            }
            let key = name.to_ascii_lowercase();
            if matches!(key.as_str(), "content-length" | "l") {
                return Err(SipRequestError::InvalidHeader);
            }
            let index = match key.as_str() {
                "via" | "v" => Some(0),
                "from" | "f" => Some(1),
                "to" | "t" => Some(2),
                "call-id" | "i" => Some(3),
                "cseq" => Some(4),
                "max-forwards" => Some(5),
                _ => None,
            };
            if let Some(index) = index {
                if required[index] {
                    return Err(SipRequestError::InvalidHeader);
                }
                required[index] = true;
            }
            if key == "cseq" {
                let mut parts = value.split_whitespace();
                let number = parts.next().ok_or(SipRequestError::InvalidCSeq)?;
                if !number.bytes().all(|byte| byte.is_ascii_digit())
                    || number
                        .parse::<u32>()
                        .ok()
                        .filter(|value| *value > 0)
                        .is_none()
                    || parts.next() != Some(self.method)
                    || parts.next().is_some()
                {
                    return Err(SipRequestError::InvalidCSeq);
                }
            }
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(b": ");
            out.extend_from_slice(value.as_bytes());
            out.extend_from_slice(b"\r\n");
            if out.len() > 65_535 {
                return Err(SipRequestError::MessageTooLarge);
            }
        }
        if required.iter().any(|present| !present) {
            return Err(SipRequestError::MissingHeader);
        }
        let content_length = self.body.len().to_string();
        let final_length = out
            .len()
            .checked_add("Content-Length: ".len())
            .and_then(|length| length.checked_add(content_length.len()))
            .and_then(|length| length.checked_add(4))
            .and_then(|length| length.checked_add(self.body.len()))
            .ok_or(SipRequestError::MessageTooLarge)?;
        if final_length > 65_535 {
            return Err(SipRequestError::MessageTooLarge);
        }
        out.extend_from_slice(b"Content-Length: ");
        out.extend_from_slice(content_length.as_bytes());
        out.extend_from_slice(b"\r\n\r\n");
        out.extend_from_slice(self.body);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_headers(method: &str) -> Vec<(&'static str, String)> {
        vec![
            (
                "Via",
                "SIP/2.0/TCP 192.0.2.1:5060;branch=z9hG4bK-test".into(),
            ),
            ("From", "<sip:user@ims.example>;tag=local".into()),
            ("To", "<sip:service@ims.example>".into()),
            ("Call-ID", "ussd-1".into()),
            ("CSeq", format!("1 {method}")),
            ("Max-Forwards", "70".into()),
        ]
    }

    #[test]
    fn encodes_signaling_request_and_binary_body_without_changing_header_order() {
        let headers = base_headers("INFO");
        let references: Vec<_> = headers
            .iter()
            .map(|(name, value)| (*name, value.as_str()))
            .collect();
        let wire = SipRequest {
            method: "INFO",
            uri: "sip:*123%23;phone-context=ims.example@ims.example;user=dialstring",
            headers: &references,
            body: &[0, 0xff],
        }
        .encode()
        .unwrap();
        assert_eq!(
            wire.as_slice(),
            b"INFO sip:*123%23;phone-context=ims.example@ims.example;user=dialstring SIP/2.0\r\nVia: SIP/2.0/TCP 192.0.2.1:5060;branch=z9hG4bK-test\r\nFrom: <sip:user@ims.example>;tag=local\r\nTo: <sip:service@ims.example>\r\nCall-ID: ussd-1\r\nCSeq: 1 INFO\r\nMax-Forwards: 70\r\nContent-Length: 2\r\n\r\n\0\xff"
        );
    }

    #[test]
    fn rejects_unsupported_methods_injection_and_mismatched_cseq() {
        let headers = base_headers("INFO");
        let references: Vec<_> = headers
            .iter()
            .map(|(name, value)| (*name, value.as_str()))
            .collect();
        assert_eq!(
            SipRequest {
                method: "OPTIONS",
                uri: "sip:ims.example",
                headers: &references,
                body: &[],
            }
            .encode(),
            Err(SipRequestError::InvalidMethod)
        );
        let mut injected = base_headers("INFO");
        injected[1].1.push_str("\r\nX-Injected: yes");
        let references: Vec<_> = injected
            .iter()
            .map(|(name, value)| (*name, value.as_str()))
            .collect();
        assert_eq!(
            SipRequest {
                method: "INFO",
                uri: "sip:ims.example",
                headers: &references,
                body: &[],
            }
            .encode(),
            Err(SipRequestError::InvalidHeader)
        );
        let mismatched = base_headers("INVITE");
        let references: Vec<_> = mismatched
            .iter()
            .map(|(name, value)| (*name, value.as_str()))
            .collect();
        assert_eq!(
            SipRequest {
                method: "INFO",
                uri: "sip:ims.example",
                headers: &references,
                body: &[],
            }
            .encode(),
            Err(SipRequestError::InvalidCSeq)
        );
    }
}
