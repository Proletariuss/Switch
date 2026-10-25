use crate::{alphabet, septets};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Framing,
    Invalid,
}

/// Parse a TP address at `offset` and return its normalized value plus the
/// first byte following the address. Numeric and standard alphanumeric
/// addresses share this implementation across DELIVER and STATUS-REPORT.
pub(crate) fn parse(tpdu: &[u8], offset: usize) -> Result<(String, usize), Error> {
    let length = usize::from(*tpdu.get(offset).ok_or(Error::Framing)?);
    let toa = *tpdu.get(offset + 1).ok_or(Error::Framing)?;
    if length == 0 || toa & 0x80 == 0 {
        return Err(Error::Invalid);
    }
    let alphanumeric = toa & 0x70 == 0x50;
    let octets = length.div_ceil(2);
    let start = offset.checked_add(2).ok_or(Error::Framing)?;
    let end = start.checked_add(octets).ok_or(Error::Framing)?;
    let bytes = tpdu.get(start..end).ok_or(Error::Framing)?;
    if alphanumeric {
        let septet_count = length * 4 / 7;
        let codes = septets::unpack(bytes, septet_count, 0).map_err(|_| Error::Invalid)?;
        let value = alphabet::decode(&codes).map_err(|_| Error::Invalid)?;
        if value.is_empty() {
            return Err(Error::Invalid);
        }
        return Ok((value, end));
    }
    if length > 20 {
        return Err(Error::Invalid);
    }
    let mut value = String::with_capacity(length + 1);
    if toa & 0x70 == 0x10 {
        value.push('+');
    }
    for index in 0..length {
        let digit = (bytes[index / 2] >> ((index % 2) * 4)) & 15;
        if digit > 9 {
            return Err(Error::Invalid);
        }
        value.push(char::from(b'0' + digit));
    }
    if length % 2 == 1 && bytes[octets - 1] >> 4 != 15 {
        return Err(Error::Invalid);
    }
    Ok((value, end))
}
