use proletarius_call::pcma::{PCM_BYTES_PER_FRAME, decode_frame, encode_frame};

fn decode_hex(value: &str) -> Result<Vec<u8>, String> {
    if !value.len().is_multiple_of(2) {
        return Err("hex input must contain complete bytes".to_owned());
    }
    (0..value.len())
        .step_by(2)
        .map(|offset| {
            u8::from_str_radix(&value[offset..offset + 2], 16)
                .map_err(|_| "invalid hex input".to_owned())
        })
        .collect()
}

fn encode_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn main() -> Result<(), String> {
    let input = std::env::args()
        .nth(1)
        .ok_or_else(|| "missing PCM frame or --all-samples".to_owned())?;
    if input == "--all-samples" {
        let pcm = (i16::MIN..=i16::MAX)
            .flat_map(|sample| sample.to_le_bytes())
            .collect::<Vec<_>>();
        let (samples, remainder) = pcm.as_chunks::<2>();
        debug_assert!(remainder.is_empty());
        let encoded = samples
            .iter()
            .map(|bytes| {
                proletarius_call::pcma::encode_sample(i16::from_le_bytes([bytes[0], bytes[1]]))
            })
            .collect::<Vec<_>>();
        let decoded = encoded
            .iter()
            .flat_map(|code| proletarius_call::pcma::decode_sample(*code).to_le_bytes())
            .collect::<Vec<_>>();
        println!("{}\n{}", encode_hex(&encoded), encode_hex(&decoded));
        return Ok(());
    }
    let bytes = decode_hex(&input)?;
    let pcm: [u8; PCM_BYTES_PER_FRAME] = bytes
        .try_into()
        .map_err(|_| format!("PCM frame must contain {PCM_BYTES_PER_FRAME} bytes"))?;
    let encoded = encode_frame(&pcm);
    let decoded = decode_frame(&encoded);
    println!("{}\n{}", encode_hex(&encoded), encode_hex(&decoded));
    Ok(())
}
