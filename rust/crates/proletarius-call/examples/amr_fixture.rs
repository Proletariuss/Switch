use proletarius_call::amr::{to_bandwidth_efficient, to_octet_aligned};

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
    let mode = std::env::args()
        .nth(1)
        .ok_or_else(|| "usage: amr_fixture <pack|unpack> <hex>".to_owned())?;
    let input = decode_hex(
        &std::env::args()
            .nth(2)
            .ok_or_else(|| "missing AMR payload".to_owned())?,
    )?;
    let output = match mode.as_str() {
        "pack" => to_bandwidth_efficient(&input)
            .map(|value| value.to_vec())
            .map_err(|error| error.to_string())?,
        "unpack" => to_octet_aligned(&input)
            .map(|value| value.to_vec())
            .map_err(|error| error.to_string())?,
        _ => return Err("mode must be pack or unpack".to_owned()),
    };
    println!("{}", encode_hex(&output));
    Ok(())
}
