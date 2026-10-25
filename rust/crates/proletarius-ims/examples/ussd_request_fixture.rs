use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use proletarius_ims::sip_request::SipRequest;

fn main() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let method = args.next().ok_or_else(|| "missing method".to_owned())?;
    let uri = args.next().ok_or_else(|| "missing uri".to_owned())?;
    let body = STANDARD
        .decode(args.next().ok_or_else(|| "missing body".to_owned())?)
        .map_err(|_| "invalid body".to_owned())?;
    let count: usize = args
        .next()
        .ok_or_else(|| "missing header count".to_owned())?
        .parse()
        .map_err(|_| "invalid header count".to_owned())?;
    if count > 120 {
        return Err("too many headers".to_owned());
    }
    let mut headers = Vec::with_capacity(count);
    for _ in 0..count {
        let value = STANDARD
            .decode(args.next().ok_or_else(|| "missing header".to_owned())?)
            .map_err(|_| "invalid header".to_owned())?;
        let value = String::from_utf8(value).map_err(|_| "invalid header utf8".to_owned())?;
        let (name, value) = value
            .split_once('\0')
            .ok_or_else(|| "invalid header pair".to_owned())?;
        headers.push((name.to_owned(), value.to_owned()));
    }
    if args.next().is_some() {
        return Err("too many arguments".to_owned());
    }
    let references: Vec<_> = headers
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    let wire = SipRequest {
        method: &method,
        uri: &uri,
        headers: &references,
        body: &body,
    }
    .encode()
    .map_err(|error| format!("invalid request: {error:?}"))?;
    println!("{}", STANDARD.encode(&wire));
    Ok(())
}
