use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use proletarius_ims::call_target::{CallTargetError, normalize};

fn decode(value: Option<String>, label: &str) -> Result<String, String> {
    let value = value.ok_or_else(|| format!("missing {label}"))?;
    let bytes = STANDARD
        .decode(value)
        .map_err(|_| format!("invalid {label}"))?;
    String::from_utf8(bytes).map_err(|_| format!("invalid {label} utf8"))
}

fn main() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let peer = decode(args.next(), "peer")?;
    let call_context = decode(args.next(), "call context")?;
    let home = decode(args.next(), "home domain")?;
    let realm = decode(args.next(), "realm")?;
    if args.next().is_some() {
        return Err("too many arguments".to_owned());
    }
    match normalize(&peer, &call_context, &home, &realm) {
        Ok(uri) => println!("ok {}", STANDARD.encode(uri)),
        Err(CallTargetError::Required) => println!("target_required"),
        Err(CallTargetError::PhoneContextRequired) => println!("phone_context_required"),
        Err(CallTargetError::EmergencyCallingDisabled) => println!("emergency_calling_disabled"),
        Err(CallTargetError::UssdUnavailable) => println!("ussd_unavailable"),
    }
    Ok(())
}
