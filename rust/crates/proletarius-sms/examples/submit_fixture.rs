//! Offline synthetic-fixture helper; never sends messages.
fn main() {
    let args: Vec<String> = std::env::args().collect();
    assert!(args.len() == 3 || args.len() == 5);
    let encoding = match args.get(3).map(String::as_str).unwrap_or("auto") {
        "auto" => proletarius_sms::submit::EncodingPolicy::Auto,
        "ucs2" => proletarius_sms::submit::EncodingPolicy::Ucs2,
        "modem" => proletarius_sms::submit::EncodingPolicy::Modem,
        _ => panic!("unknown fixture encoding"),
    };
    let request_status_report = args.get(4).is_none_or(|value| value == "true");
    let parts = proletarius_sms::submit::prepare_with_policy(
        &args[1],
        &args[2],
        42,
        encoding,
        request_status_report,
    )
    .expect("fixture encode");
    for part in parts {
        for byte in part {
            print!("{byte:02x}");
        }
        println!();
    }
}
