//! Bounded user-data-header parsing. Unknown information elements are retained.
use crate::SmsError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Concatenation {
    pub reference: u16,
    pub wide_reference: bool,
    pub total: u8,
    pub sequence: u8,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Header<'a> {
    pub octets: usize,
    pub concatenation: Option<Concatenation>,
    pub elements: Vec<(u8, &'a [u8])>,
}

pub fn parse(user_data: &[u8]) -> Result<Header<'_>, SmsError> {
    let length = usize::from(*user_data.first().ok_or(SmsError::Truncated("UDHL"))?) + 1;
    if length > 140 {
        return Err(SmsError::InvalidUserDataLength);
    }
    let bytes = user_data.get(..length).ok_or(SmsError::Truncated("UDH"))?;
    let mut result = Header {
        octets: length,
        concatenation: None,
        elements: Vec::new(),
    };
    let mut offset = 1;
    while offset < length {
        let id = bytes[offset];
        let size = usize::from(
            *bytes
                .get(offset + 1)
                .ok_or(SmsError::Truncated("UDH element length"))?,
        );
        offset += 2;
        let data = bytes
            .get(offset..offset + size)
            .ok_or(SmsError::Truncated("UDH element"))?;
        offset += size;
        if id == 0 || id == 8 {
            if result.concatenation.is_some() || (id == 0 && size != 3) || (id == 8 && size != 4) {
                return Err(SmsError::InvalidUserDataLength);
            }
            let wide = id == 8;
            let index = if wide { 2 } else { 1 };
            let total = data[index];
            let sequence = data[index + 1];
            if total == 0 || sequence == 0 || sequence > total {
                return Err(SmsError::InvalidUserDataLength);
            }
            result.concatenation = Some(Concatenation {
                reference: if wide {
                    u16::from_be_bytes([data[0], data[1]])
                } else {
                    u16::from(data[0])
                },
                wide_reference: wide,
                total,
                sequence,
            });
        }
        result.elements.push((id, data));
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn distinguishes_reference_width_and_preserves_other_elements() {
        let narrow = parse(&[5, 0, 3, 42, 2, 1, 99]).unwrap();
        assert_eq!(narrow.octets, 6);
        assert_eq!(
            narrow.concatenation.unwrap(),
            Concatenation {
                reference: 42,
                wide_reference: false,
                total: 2,
                sequence: 1
            }
        );
        let wide = parse(&[9, 8, 4, 0, 42, 2, 1, 0x24, 1, 1]).unwrap();
        assert!(wide.concatenation.unwrap().wide_reference);
        assert_eq!(wide.elements[1], (0x24, &[1][..]));
    }
    #[test]
    fn rejects_invalid_and_ambiguous_headers() {
        for data in [
            vec![],
            vec![5, 0, 3],
            vec![1, 0],
            vec![5, 0, 3, 42, 0, 0],
            vec![5, 0, 3, 42, 2, 3],
            vec![10, 0, 3, 42, 2, 1, 0, 3, 43, 2, 1],
        ] {
            assert!(parse(&data).is_err());
        }
        assert_eq!(parse(&[0]).unwrap().octets, 1);
    }
}
