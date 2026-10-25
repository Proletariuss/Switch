use crate::filesystem::{Helper, Key, PidFile, Socket};
use crate::owner::ChildOwner;
use crate::protocol::{MAX_LINE, valid_session, verify_request};
use crate::{Command, Error, State, encode_request, encode_response, verify_response};
use std::io::{Read, Write};
use std::net::Shutdown;
use std::os::fd::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::time::{Duration, Instant};

const IO_BUDGET: Duration = Duration::from_secs(1);

/// Paths are existing private directories. All session artifacts are retained
/// after RELEASE, including the original child PID diagnostic file.
pub struct ServeConfig {
    pub socket: PathBuf,
    pub key_file: PathBuf,
    pub session: String,
    pub helper: PathBuf,
    pub child_pid_file: PathBuf,
}
pub struct ClientConfig {
    pub socket: PathBuf,
    pub key_file: PathBuf,
    pub session: String,
    pub nonce: String,
    pub command: Command,
}

pub fn serve(config: ServeConfig) -> Result<(), Error> {
    if !valid_session(&config.session) {
        return Err(Error::Protocol);
    }
    let key = Key::open(&config.key_file)?;
    let helper = Helper::open(&config.helper)?;
    let mut pid_file = PidFile::reserve(&config.child_pid_file)?;
    let (socket, listener) = Socket::bind(&config.socket)?;
    listener.set_nonblocking(true)?;
    key.validate()?;
    socket.validate()?;
    let command = helper.command()?;
    let owner = ChildOwner::spawn(command)?;
    // From here the actual Child stays in the owner loop. Errors writing the
    // diagnostic file or checking names never drop a live Child or signal a
    // PID recovered from a file. A later authenticated STOP remains possible.
    let mut owner = owner;
    let _ = pid_file.write(owner.pid());
    let _ = helper.validate();
    run_owner(&config.session, &key, &socket, listener, &mut owner)
}

fn run_owner(
    session: &str,
    key: &Key,
    socket: &Socket,
    listener: UnixListener,
    owner: &mut ChildOwner,
) -> Result<(), Error> {
    // This function may return only after terminal RELEASE was signed and its
    // complete response written. Terminal STOP/STATUS does not retire it.
    loop {
        let _ = owner.poll();
        match listener.accept() {
            Ok((mut stream, _)) => {
                // Explicit on every platform: poll readiness followed by
                // nonblocking I/O shares one absolute deadline per record.
                if stream.set_nonblocking(true).is_err() {
                    continue; // Keep the actual child owner; drop only this peer.
                }
                let outcome = handle_request(session, key, socket, owner, &mut stream);
                if matches!(outcome, Ok(true)) {
                    // handle_request requires an observed wait result before
                    // signing RELEASE. No name is unlinked: a check/unlink
                    // race could otherwise remove a replacement object.
                    return Ok(());
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => {
                // Keep ownership even on a broken listener. This is pending,
                // never a fabricated terminal result or automatic restart.
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

fn handle_request(
    session: &str,
    key: &Key,
    socket: &Socket,
    owner: &mut ChildOwner,
    stream: &mut UnixStream,
) -> Result<bool, Error> {
    key.validate()?;
    socket.validate()?;
    let line = read_line(stream, Instant::now() + IO_BUDGET)?;
    let request = verify_request(&line, session, key.bytes())?;
    // Validate the original namespace again after potentially blocking I/O.
    // Failure does not release the child owner or redirect to a new socket.
    key.validate()?;
    socket.validate()?;
    let state = match request.command {
        Command::Stop => owner.stop_once()?,
        Command::Status | Command::Release => owner.poll()?,
    };
    let response = encode_response(
        session,
        request.nonce,
        request.command,
        state,
        owner.pid(),
        owner.exit(),
        key.bytes(),
    )?;
    write_line(stream, response.as_bytes(), Instant::now() + IO_BUDGET)?;
    let _ = stream.shutdown(Shutdown::Both);
    // If the caller loses a RELEASE ACK, it must keep an unknown result;
    // successful write is not proof that the remote caller received it.
    Ok(request.command == Command::Release && state == State::Exited)
}

pub fn client(config: ClientConfig) -> Result<String, Error> {
    let key = Key::open(&config.key_file)?;
    let request = encode_request(&config.session, &config.nonce, config.command, key.bytes())?;
    let socket = Socket::existing(&config.socket)?;
    let mut stream = socket.connect()?;
    stream.set_nonblocking(true)?;
    key.validate()?;
    socket.validate()?;
    write_line(&mut stream, request.as_bytes(), Instant::now() + IO_BUDGET)?;
    stream.shutdown(Shutdown::Write)?;
    let response = read_line(&mut stream, Instant::now() + IO_BUDGET)?;
    key.validate()?;
    socket.validate()?;
    verify_response(
        &response,
        &config.session,
        &config.nonce,
        config.command,
        key.bytes(),
    )?;
    String::from_utf8(response).map_err(|_| Error::Protocol)
}

// Never change SO_RCVTIMEO/SO_SNDTIMEO while consuming a half-closed
// stream: Darwin rejects option changes once both directions are shut down.
// POLLHUP/POLLERR only permit a real read/write attempt, never success by
// themselves. In particular buffered signed bytes must drain before EOF.
fn wait_ready(stream: &UnixStream, events: libc::c_short, deadline: Instant) -> Result<(), Error> {
    loop {
        let left = deadline
            .checked_duration_since(Instant::now())
            .ok_or(Error::Io)?;
        if left.is_zero() {
            return Err(Error::Io);
        }
        let nanos = left.as_nanos();
        let millis = nanos.div_ceil(1_000_000).min(i32::MAX as u128) as i32;
        let mut descriptor = libc::pollfd {
            fd: stream.as_raw_fd(),
            events,
            revents: 0,
        };
        // SAFETY: poll borrows one initialized descriptor for this call only;
        // the actual stream remains owned and open through every I/O attempt.
        let result = unsafe { libc::poll(&mut descriptor, 1, millis) };
        if result < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(Error::Io);
        }
        if Instant::now() >= deadline {
            return Err(Error::Io);
        }
        if result == 0 {
            continue;
        }
        if descriptor.revents & libc::POLLNVAL != 0 {
            return Err(Error::Io);
        }
        if descriptor.revents & (events | libc::POLLHUP | libc::POLLERR) != 0 {
            return Ok(());
        }
    }
}
fn read_line(stream: &mut UnixStream, deadline: Instant) -> Result<Vec<u8>, Error> {
    // Also supports ordinary blocking UnixStream::pair() callers in tests.
    // FIONBIO does not mutate socket timeout options after peer shutdown.
    stream.set_nonblocking(true)?;
    let mut bytes = Vec::with_capacity(MAX_LINE);
    loop {
        wait_ready(stream, libc::POLLIN, deadline)?;
        let mut block = [0u8; MAX_LINE + 1];
        match stream.read(&mut block) {
            Ok(0) => {
                // Bare EOF is never a successful protocol response. A whole
                // bounded record and later MAC/scope validation are required.
                return if bytes.last() == Some(&b'\n') {
                    Ok(bytes)
                } else {
                    Err(Error::Protocol)
                };
            }
            Ok(count) => {
                if bytes.len() + count > MAX_LINE {
                    return Err(Error::Protocol);
                }
                bytes.extend_from_slice(&block[..count]);
                if let Some(end) = bytes.iter().position(|b| *b == b'\n') {
                    if end + 1 != bytes.len() {
                        return Err(Error::Protocol);
                    }
                    // Still require write-half EOF. A second record in a
                    // later packet is rejected under this same deadline.
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::Interrupted | std::io::ErrorKind::WouldBlock
                ) => {}
            Err(_) => return Err(Error::Io),
        }
    }
}
fn write_line(stream: &mut UnixStream, bytes: &[u8], deadline: Instant) -> Result<(), Error> {
    if bytes.len() > MAX_LINE {
        return Err(Error::Protocol);
    }
    stream.set_nonblocking(true)?;
    let mut written = 0;
    while written < bytes.len() {
        wait_ready(stream, libc::POLLOUT, deadline)?;
        match stream.write(&bytes[written..]) {
            Ok(0) => return Err(Error::Io),
            Ok(count) => written += count,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::Interrupted | std::io::ErrorKind::WouldBlock
                ) => {}
            Err(_) => return Err(Error::Io),
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "server_tests.rs"]
mod tests;

#[cfg(test)]
#[test]
fn complete_signed_record_is_read_after_both_socket_directions_are_shutdown() {
    let (mut sender, mut receiver) = UnixStream::pair().unwrap();
    receiver.shutdown(Shutdown::Write).unwrap();
    let session = "00112233445566778899aabbccddeeff";
    let nonce = "a".repeat(64);
    let key = [7u8; 32];
    let wire = encode_response(
        session,
        &nonce,
        Command::Stop,
        State::Exited,
        123,
        crate::Exit::Code(0),
        &key,
    )
    .unwrap();
    sender.write_all(wire.as_bytes()).unwrap();
    match sender.shutdown(Shutdown::Both) {
        Ok(()) => {}
        // Darwin may already consider the pair disconnected after the
        // receiver write-half closed. Dropping sender below still ends its
        // write half; all signed bytes and actual EOF remain required.
        Err(error) if error.kind() == std::io::ErrorKind::NotConnected => {}
        Err(error) => panic!("unexpected fixture shutdown failure: {error}"),
    }
    drop(sender);
    // Both shutdown flags can already be set here. The real read must drain
    // the signed bytes then observe EOF, without SO_RCVTIMEO reconfiguration.
    let received = read_line(&mut receiver, Instant::now() + IO_BUDGET).unwrap();
    let verified = verify_response(&received, session, &nonce, Command::Stop, &key).unwrap();
    assert_eq!(verified.state(), State::Exited);
    assert_eq!(received, wire.as_bytes());
}
