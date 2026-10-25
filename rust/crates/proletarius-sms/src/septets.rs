//! Bounded GSM-7 bit packing for a single TP-UD, including UDH fill bits.
use crate::SmsError;

const MAX_BITS: usize = 140 * 8;

fn end_bit(count: usize, start: usize) -> Result<usize, SmsError> {
    count
        .checked_mul(7)
        .and_then(|n| start.checked_add(n))
        .filter(|n| *n <= MAX_BITS)
        .ok_or(SmsError::InvalidUserDataLength)
}

pub fn pack(values: &[u8], start: usize) -> Result<Vec<u8>, SmsError> {
    let end = end_bit(values.len(), start)?;
    if values.iter().any(|v| *v > 127) {
        return Err(SmsError::InvalidUserDataLength);
    }
    let mut result = vec![0; end.div_ceil(8)];
    for (index, &value) in values.iter().enumerate() {
        let bit = start + index * 7;
        let shift = bit % 8;
        result[bit / 8] |= value << shift;
        if shift > 1 {
            result[bit / 8 + 1] |= value >> (8 - shift);
        }
    }
    Ok(result)
}

pub fn unpack(data: &[u8], count: usize, start: usize) -> Result<Vec<u8>, SmsError> {
    let end = end_bit(count, start)?;
    if data.len() > 140 || end > data.len() * 8 {
        return Err(SmsError::InvalidUserDataLength);
    }
    let mut result = Vec::with_capacity(count);
    for index in 0..count {
        let bit = start + index * 7;
        let shift = bit % 8;
        let mut value = data[bit / 8] >> shift;
        if shift > 1 {
            value |= data[bit / 8 + 1] << (8 - shift);
        }
        result.push(value & 127);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_balance_request_vector() {
        assert_eq!(pack(b"BAL", 0).unwrap(), [0xc2, 0x20, 0x13]);
    }

    #[test]
    fn all_values_and_alignments_roundtrip() {
        let values: Vec<u8> = (0..128).collect();
        for start in 0..56 {
            let packed = pack(&values, start).unwrap();
            assert_eq!(unpack(&packed, values.len(), start).unwrap(), values);
            for bit in 0..start {
                assert_eq!(packed[bit / 8] & (1 << (bit % 8)), 0);
            }
        }
        let multipart = vec![127; 153];
        let packed = pack(&multipart, 49).unwrap();
        assert_eq!(packed.len(), 140);
        assert_eq!(unpack(&packed, 153, 49).unwrap(), multipart);
    }

    #[test]
    fn malformed_lengths_never_allocate_unbounded_buffers() {
        assert!(pack(&[128], 0).is_err());
        assert!(pack(&[1], usize::MAX).is_err());
        assert!(pack(&[1; 161], 0).is_err());
        assert!(unpack(&[0], usize::MAX, 0).is_err());
        assert!(unpack(&[], 1, 0).is_err());
        assert!(unpack(&[0; 141], 0, 0).is_err());
    }
}
