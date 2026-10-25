use std::path::PathBuf;
use switch_voice_supervisor::{ClientConfig, Command, Error, ServeConfig};

fn options() -> Result<(String, std::collections::BTreeMap<String, String>), Error> {
    let mut args = std::env::args().skip(1);
    let mode = args.next().ok_or(Error::Protocol)?;
    if !matches!(mode.as_str(), "serve" | "client") {
        return Err(Error::Protocol);
    }
    let mut values = std::collections::BTreeMap::new();
    while let Some(flag) = args.next() {
        if !flag.starts_with("--") {
            return Err(Error::Protocol);
        }
        let value = args.next().ok_or(Error::Protocol)?;
        if value.starts_with("--") || values.insert(flag, value).is_some() {
            return Err(Error::Protocol);
        }
    }
    Ok((mode, values))
}
fn run() -> Result<(), Error> {
    let (mode, mut options) = options()?;
    let mut take = |flag: &str| options.remove(flag).ok_or(Error::Protocol);
    let socket = PathBuf::from(take("--socket")?);
    let key_file = PathBuf::from(take("--key-file")?);
    let session = take("--session")?;
    if mode == "serve" {
        let helper = PathBuf::from(take("--helper")?);
        let child_pid_file = PathBuf::from(take("--child-pid-file")?);
        if !options.is_empty() {
            return Err(Error::Protocol);
        }
        switch_voice_supervisor::serve(ServeConfig {
            socket,
            key_file,
            session,
            helper,
            child_pid_file,
        })
    } else {
        let nonce = take("--nonce")?;
        let command = match take("--command")?.as_str() {
            "status" => Command::Status,
            "stop" => Command::Stop,
            "release" => Command::Release,
            _ => return Err(Error::Protocol),
        };
        if !options.is_empty() {
            return Err(Error::Protocol);
        }
        let response = switch_voice_supervisor::client(ClientConfig {
            socket,
            key_file,
            session,
            nonce,
            command,
        })?;
        use std::io::Write;
        std::io::stdout()
            .lock()
            .write_all(response.as_bytes())
            .map_err(|_| Error::Io)
    }
}
fn main() {
    if let Err(error) = run() {
        // Only fixed errors are emitted. Never print a key, MAC, shell command
        // or raw path/error supplied by another process.
        eprintln!("{}", error.code());
        std::process::exit(1);
    }
}
