// Copyright (c) 2026 iniwex5
// Modifications copyright (c) 2026 anti-rainer
// SPDX-License-Identifier: MIT

//! The QMI transport contract. Platform USB discovery and ownership are
//! supplied by the caller; `RawTransport` is for an already-open character
//! device and protocol fixtures.

use std::io::{self, Read, Write};
use std::path::Path;
use std::time::{Duration, Instant};

pub trait Transport: Send + Sync {
    fn read(&self, buffer: &mut [u8]) -> io::Result<usize>;
    fn write_all(&self, data: &[u8]) -> io::Result<()>;
    /// Write with the caller's remaining total request budget. Implementers
    /// that may block must override this fallback and bound their own waits.
    /// Failure can follow a partial/full write; it is not a non-submission ACK.
    /// The core checks its absolute deadline again after this call returns,
    /// but cannot interrupt a synchronous implementation that ignores timeout.
    fn write_all_timeout(&self, data: &[u8], _timeout: Duration) -> io::Result<()> {
        self.write_all(data)
    }
    fn shutdown(&self) -> io::Result<()>;

    /// Duplicate the control channel so one owner can keep a read pending
    /// while another writes. A QMI transport is a single serial resource: a
    /// reader that holds the only handle across its poll blocks every writer
    /// behind it, which is why the proxy uses one duplicate per direction.
    fn try_clone(&self) -> io::Result<Box<dyn Transport>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "this QMI transport cannot be duplicated",
        ))
    }

    /// The module's data function, when this transport also owns it.
    ///
    /// A module multiplexes its QMI channel and its data muxes over one USB
    /// interface, so a data port has to be taken from the same claim the
    /// control channel holds. A transport that reaches the module through
    /// something else - a character device on a host that has a netdev - has
    /// no port to hand out and answers `None`.
    fn packet_port(&self, _mux_id: u8) -> Option<std::sync::Arc<dyn PacketPort>> {
        None
    }

    /// The module's data function when it carries plain IP.
    ///
    /// A firmware that does not aggregate hands whole IP packets to the bulk
    /// pair, with no mux and no QMAP header - the form Linux calls "native".
    /// The same function, the same claim, only the framing differs, so it is
    /// the same kind of capability [`Self::packet_port`] hands out.
    fn native_packet_port(&self) -> Option<std::sync::Arc<dyn PacketPort>> {
        None
    }
}

/// One mux of a module's data function.
pub trait PacketPort: Send + Sync {
    /// Send one IP packet on this mux.
    fn send(&self, packet: &[u8]) -> io::Result<()>;
    /// Receive one IP packet belonging to this mux, or `None` if `timeout`
    /// passed without one. On a transport that multiplexes several muxes onto
    /// one pipe the frames are demultiplexed below this boundary, so a port
    /// never sees - or drops - another mux's packets.
    fn receive(&self, output: &mut [u8], timeout: Duration) -> io::Result<Option<usize>>;
}

pub struct RawTransport {
    file: std::fs::File,
}

impl RawTransport {
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true);
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::OpenOptionsExt;

            const O_NOCTTY: i32 = 0x100;
            const O_NONBLOCK: i32 = 0x800;
            options.custom_flags(O_NOCTTY | O_NONBLOCK);
        }
        Ok(Self {
            file: options.open(path)?,
        })
    }
}

impl Transport for RawTransport {
    fn read(&self, buffer: &mut [u8]) -> io::Result<usize> {
        (&self.file).read(buffer)
    }

    fn write_all(&self, data: &[u8]) -> io::Result<()> {
        self.write_all_timeout(data, Duration::from_secs(30))
    }

    fn write_all_timeout(&self, data: &[u8], timeout: Duration) -> io::Result<()> {
        let started = Instant::now();
        let mut offset = 0;
        while offset < data.len() {
            if started.elapsed() >= timeout {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "QMI control-channel write timed out",
                ));
            }
            match (&self.file).write(&data[offset..]) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "QMI control channel accepted no data",
                    ));
                }
                Ok(written) => offset += written,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    let remaining = timeout.saturating_sub(started.elapsed());
                    if remaining.is_zero() {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "QMI control-channel write timed out",
                        ));
                    }
                    std::thread::yield_now();
                }
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    fn shutdown(&self) -> io::Result<()> {
        Ok(())
    }

    fn try_clone(&self) -> io::Result<Box<dyn Transport>> {
        Ok(Box::new(Self {
            file: self.file.try_clone()?,
        }))
    }
}
