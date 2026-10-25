use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() != 4 {
        std::process::exit(2);
    }
    let Ok(status) = args[0].parse::<u16>() else {
        std::process::exit(2);
    };
    let decoded: Result<Vec<_>, _> = args[1..]
        .iter()
        .map(|value| STANDARD.decode(value))
        .collect();
    let Ok(decoded) = decoded else {
        std::process::exit(2);
    };
    let strings: Result<Vec<_>, _> = decoded
        .iter()
        .map(|value| String::from_utf8(value.clone()))
        .collect();
    let Ok(strings) = strings else {
        std::process::exit(2);
    };
    match proletarius_ims::call_rejection::rejection_message(
        status,
        &strings[0],
        &strings[1],
        &strings[2],
    ) {
        Ok(message) => println!("ok {}", STANDARD.encode(message.as_bytes())),
        Err(error) => {
            eprintln!("{error:?}");
            std::process::exit(1);
        }
    }
}
