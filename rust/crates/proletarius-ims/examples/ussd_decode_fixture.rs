use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use proletarius_ims::ussd_xml::{UssdContentError, decode_content};

fn main() -> Result<(), String> {
    let mut arguments = std::env::args().skip(1);
    let content_type = arguments
        .next()
        .ok_or_else(|| "missing content type".to_owned())?;
    let encoded_body = arguments
        .next()
        .ok_or_else(|| "missing base64 body".to_owned())?;
    if arguments.next().is_some() {
        return Err("too many arguments".to_owned());
    }
    let body = STANDARD
        .decode(encoded_body)
        .map_err(|_| "invalid base64 body".to_owned())?;
    match decode_content(&content_type, &body) {
        Ok(document) => println!(
            "ok|{}|{}",
            STANDARD.encode(document.language),
            STANDARD.encode(document.text)
        ),
        Err(error) => println!("error|{}", category(error)),
    }
    Ok(())
}

fn category(error: UssdContentError) -> &'static str {
    match error {
        UssdContentError::ResponseTooLarge => "response_too_large",
        UssdContentError::InvalidContentType => "invalid_content_type",
        UssdContentError::MissingContent => "missing_content",
        UssdContentError::UnsupportedContentType => "unsupported_content_type",
        UssdContentError::InvalidXml => "invalid_xml",
        UssdContentError::CarrierError => "carrier_error",
    }
}
