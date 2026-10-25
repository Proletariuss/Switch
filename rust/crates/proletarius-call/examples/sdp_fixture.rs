use proletarius_call::sdp::{AudioCodec, negotiate_audio_with_modes};

fn main() -> Result<(), String> {
    let answer = match std::env::args().nth(1).as_deref() {
        Some("answer") => true,
        Some("offer") => false,
        _ => return Err("usage: sdp_fixture <offer|answer> <sdp>".to_owned()),
    };
    let body = std::env::args()
        .nth(2)
        .ok_or_else(|| "missing SDP body".to_owned())?;
    // Admission corpus declares codec capabilities explicitly; this tests SDP
    // policy independently of host native linking. Native codec goldens cover
    // actual EVS/AMR-WB availability and decoded audio separately.
    let (evs_available, amr_wb_modes) = match std::env::args().nth(3).as_deref() {
        Some("--fixture-wideband-capabilities") => (true, (0..=8).collect()),
        None => proletarius_call::evs::available_audio_codecs(false),
        Some(_) => return Err("unsupported fixture capability option".to_owned()),
    };
    match negotiate_audio_with_modes(&body, answer, evs_available, &amr_wb_modes) {
        Ok(result) => {
            let (codec, octet_aligned, wb_mode) = match result.codec {
                AudioCodec::AmrWb {
                    octet_aligned,
                    mode,
                    ..
                } => ("AMR-WB", octet_aligned, mode.to_string()),
                AudioCodec::Evs { .. } => ("EVS", false, "-".to_owned()),
            };
            println!(
                "ok|{}|{}|{}|{}|{}|{}|{}",
                result.remote_ip,
                result.remote_port,
                result.payload_type,
                codec,
                octet_aligned,
                result
                    .telephone_event_payload
                    .map_or_else(|| "-".to_owned(), |value| value.to_string()),
                wb_mode
            );
        }
        Err(error) => println!("error|{error}"),
    }
    Ok(())
}
