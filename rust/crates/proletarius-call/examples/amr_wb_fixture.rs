use proletarius_call::amr_wb::{depacketize, packetize};

fn from_hex(value: &str) -> Result<Vec<u8>, String> {
    if !value.len().is_multiple_of(2) {
        return Err("odd hex length".into());
    }
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let text = std::str::from_utf8(pair).map_err(|e| e.to_string())?;
            u8::from_str_radix(text, 16).map_err(|e| e.to_string())
        })
        .collect()
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|value| format!("{value:02x}")).collect()
}

fn main() -> Result<(), String> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.len() != 3 {
        return Err("usage: amr_wb_fixture <mode> <aligned:0|1> <hex-bits>".into());
    }
    let mode = args[0].parse::<u8>().map_err(|e| e.to_string())?;
    let aligned = match args[1].as_str() {
        "0" => false,
        "1" => true,
        _ => return Err("alignment must be 0 or 1".into()),
    };
    let bits = from_hex(&args[2])?;
    let packet = packetize(&bits, mode, aligned).map_err(|e| e.to_string())?;
    let decoded = depacketize(&packet, aligned).map_err(|e| e.to_string())?;
    println!(
        "{}|{}|{}|{}",
        to_hex(&packet),
        decoded.mode,
        decoded.good,
        to_hex(&decoded.bits)
    );
    Ok(())
}
