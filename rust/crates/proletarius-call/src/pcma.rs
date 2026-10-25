//! G.711 A-law for the reader's 8 kHz mono / 20 ms media contract.
//! No resampling, allocation, native codec state or hardware ownership here.

pub const SAMPLE_RATE: u32 = 8_000;
pub const SAMPLES_PER_FRAME: usize = 160;
pub const PCM_BYTES_PER_FRAME: usize = SAMPLES_PER_FRAME * 2;
pub const STATIC_PAYLOAD_TYPE: u8 = 8;

/// Input is signed 16-bit linear PCM; use a wider magnitude for i16::MIN.
pub fn encode_sample(sample: i16) -> u8 {
    let magnitude = if sample < 0 {
        -i32::from(sample) - 1
    } else {
        i32::from(sample)
    };
    let sign = if sample < 0 { 0 } else { 0x80 };
    let segment = if magnitude < 256 {
        0
    } else {
        24 - magnitude.leading_zeros()
    };
    let shift = if segment == 0 { 4 } else { segment + 3 };
    (sign | ((segment as u8) << 4) | (((magnitude >> shift) & 15) as u8)) ^ 0x55
}

pub fn decode_sample(encoded: u8) -> i16 {
    let value = encoded ^ 0x55;
    let segment = (value >> 4) & 7;
    let base = i32::from(value & 15) << 4;
    let magnitude = if segment == 0 {
        base + 8
    } else {
        (base + 264) << (segment - 1)
    };
    if value & 0x80 == 0 {
        -magnitude as i16
    } else {
        magnitude as i16
    }
}

/// The Swift audio bridge emits little-endian PCM, independent of host endian.
pub fn encode_frame(pcm: &[u8; PCM_BYTES_PER_FRAME]) -> [u8; SAMPLES_PER_FRAME] {
    std::array::from_fn(|index| {
        encode_sample(i16::from_le_bytes([pcm[index * 2], pcm[index * 2 + 1]]))
    })
}

pub fn decode_frame(payload: &[u8; SAMPLES_PER_FRAME]) -> [u8; PCM_BYTES_PER_FRAME] {
    let mut pcm = [0; PCM_BYTES_PER_FRAME];
    for (sample, bytes) in payload.iter().zip(pcm.as_chunks_mut::<2>().0) {
        bytes.copy_from_slice(&decode_sample(*sample).to_le_bytes());
    }
    pcm
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_segment_boundaries_and_extremes() {
        for (pcm, code) in [
            (0, 0xd5),
            (-1, 0x55),
            (255, 0xda),
            (256, 0xc5),
            (512, 0xf5),
            (1024, 0xe5),
            (32767, 0xaa),
            (-32768, 0x2a),
        ] {
            assert_eq!(encode_sample(pcm), code, "sample {pcm}");
        }
    }

    #[test]
    fn every_codeword_roundtrips() {
        for code in 0..=255u8 {
            assert_eq!(encode_sample(decode_sample(code)), code);
        }
    }

    #[test]
    fn exhaustive_pcm_is_monotonic_and_quantization_is_bounded() {
        let mut previous = i32::MIN;
        for input in i16::MIN..=i16::MAX {
            let decoded = i32::from(decode_sample(encode_sample(input)));
            assert!(decoded >= previous, "nonmonotonic at {input}");
            assert!(
                (decoded - i32::from(input)).abs() <= 512,
                "error at {input}"
            );
            previous = decoded;
        }
    }

    #[test]
    fn frame_contract_preserves_little_endian_samples() {
        let pcm = std::array::from_fn(|index| if index % 2 == 0 { 0 } else { 4 });
        let payload = encode_frame(&pcm);
        assert_eq!(payload, [0xe5; SAMPLES_PER_FRAME]);
        let decoded = decode_frame(&payload);
        for sample in decoded.as_chunks::<2>().0 {
            assert_eq!(i16::from_le_bytes([sample[0], sample[1]]), 1056);
        }
    }
}
