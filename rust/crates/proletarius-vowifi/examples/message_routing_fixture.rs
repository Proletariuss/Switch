//! Offline differential fixture; never opens a network or reader.
use proletarius_ims::SipResponse;
use proletarius_vowifi::message_routing::RegisteredMessageRouting;
use serde::{Deserialize, Serialize};
use std::io::{self, Read};
#[derive(Deserialize)]
struct Input {
    configured: String,
    associated: Vec<String>,
    routes: Vec<String>,
    fallback_routes: Vec<String>,
}
#[derive(Serialize)]
struct Output {
    identity: String,
    routes: Vec<String>,
    registered_msisdn: String,
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut input = String::new();
    io::stdin().take(1_048_577).read_to_string(&mut input)?;
    if input.len() > 1_048_576 {
        return Err("fixture input too large".into());
    }
    let input: Input = serde_json::from_str(&input)?;
    let mut wire = "SIP/2.0 200 OK\r\n".to_owned();
    for value in &input.associated {
        wire.push_str(&format!("P-Associated-URI: {value}\r\n"));
    }
    for value in &input.routes {
        wire.push_str(&format!("Service-Route: {value}\r\n"));
    }
    wire.push_str("Content-Length: 0\r\n\r\n");
    let response = SipResponse::parse(wire.as_bytes())?;
    let routing = RegisteredMessageRouting::from_response(
        &response,
        &input.configured,
        &input.fallback_routes,
    )?;
    println!(
        "{}",
        serde_json::to_string(&Output {
            identity: routing.identity().into(),
            routes: routing.routes().to_vec(),
            registered_msisdn: routing.registered_msisdn().unwrap_or_default().into(),
        })?
    );
    Ok(())
}
