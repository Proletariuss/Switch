//! Bounded, best-effort diagnostics. A stalled launcher must not stall an owner.

use std::{
    fmt::{self, Write as _},
    fs::File,
    io::{self, Write},
    os::fd::AsFd,
    sync::{OnceLock, mpsc},
    time::Duration,
};

const QUEUE_CAPACITY: usize = 64;
const MAX_LINE_BYTES: usize = 4096;
const TRUNCATED: &str = " [truncated]";

enum Record {
    Line(String),
    Flush(mpsc::SyncSender<()>),
}

struct Sink(mpsc::SyncSender<Record>);

impl Sink {
    fn write_line(&self, arguments: fmt::Arguments<'_>) {
        // Both formatting and queue memory are bounded; a full/disconnected
        // queue drops this diagnostic instead of waiting for the writer.
        let _ = self.0.try_send(Record::Line(format_line(arguments)));
    }

    fn flush(&self, timeout: Duration) -> bool {
        let (ack, received) = mpsc::sync_channel(1);
        if self.0.try_send(Record::Flush(ack)).is_err() {
            return false;
        }
        received.recv_timeout(timeout).is_ok()
    }
}

fn sink() -> Option<&'static Sink> {
    static SINK: OnceLock<Option<Sink>> = OnceLock::new();
    SINK.get_or_init(|| {
        // Duplicate the descriptor, without taking std's global stderr lock.
        // Only this detached thread may block on the actual output sink.
        let output = File::from(io::stderr().as_fd().try_clone_to_owned().ok()?);
        let (sender, receiver) = mpsc::sync_channel(QUEUE_CAPACITY);
        std::thread::Builder::new()
            .name("diagnostics".into())
            .spawn(move || write_records(output, receiver))
            .ok()?;
        Some(Sink(sender))
    })
    .as_ref()
}

pub(crate) fn write_line(arguments: fmt::Arguments<'_>) {
    if let Some(sink) = sink() {
        sink.write_line(arguments);
    }
}

/// Preserve final diagnostics when the sink is available, while bounding exit
/// latency when it is full. Never join the possibly blocked writer thread.
pub(crate) fn flush(timeout: Duration) -> bool {
    sink().is_some_and(|sink| sink.flush(timeout))
}

fn format_line(arguments: fmt::Arguments<'_>) -> String {
    struct BoundedLine(String);
    impl fmt::Write for BoundedLine {
        fn write_str(&mut self, value: &str) -> fmt::Result {
            let remaining = MAX_LINE_BYTES - 1 - self.0.len();
            if value.len() <= remaining {
                self.0.push_str(value);
                return Ok(());
            }
            let mut boundary = remaining;
            while !value.is_char_boundary(boundary) {
                boundary -= 1;
            }
            self.0.push_str(&value[..boundary]);
            Err(fmt::Error)
        }
    }
    let mut line = BoundedLine(String::with_capacity(MAX_LINE_BYTES));
    if line.write_fmt(arguments).is_err() {
        let mut boundary = line.0.len().min(MAX_LINE_BYTES - 1 - TRUNCATED.len());
        while !line.0.is_char_boundary(boundary) {
            boundary -= 1;
        }
        line.0.truncate(boundary);
        line.0.push_str(TRUNCATED);
    }
    line.0.push('\n');
    line.0
}

fn write_records(mut output: impl Write, receiver: mpsc::Receiver<Record>) {
    while let Ok(record) = receiver.recv() {
        match record {
            Record::Line(line) => {
                // Closed/erroring sinks lose diagnostics, without a fallback
                // stream, retry loop or panic in a communication owner.
                let _ = output.write_all(line.as_bytes());
            }
            Record::Flush(ack) => {
                let _ = output.flush();
                let _ = ack.try_send(());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn successful_diagnostic_preserves_format_and_newline() {
        assert_eq!(
            format_line(format_args!("owner {}: {}", 7, "finished")),
            "owner 7: finished\n"
        );
    }

    #[test]
    fn oversized_unicode_and_formatter_output_are_bounded() {
        let line = format_line(format_args!("{}", "界🛰️".repeat(MAX_LINE_BYTES)));
        assert!(line.len() <= MAX_LINE_BYTES);
        assert!(line.ends_with(" [truncated]\n"));
        assert!(std::str::from_utf8(line.as_bytes()).is_ok());
        struct Many;
        impl fmt::Display for Many {
            fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
                for _ in 0..1_000_000 {
                    out.write_str("frame")?;
                }
                Ok(())
            }
        }
        assert!(format_line(format_args!("{Many}")).len() <= MAX_LINE_BYTES);
    }

    #[test]
    fn saturated_queue_drops_without_waiting_or_replacing_accepted_records() {
        let (sender, receiver) = mpsc::sync_channel(2);
        let sink = Sink(sender);
        sink.write_line(format_args!("first"));
        sink.write_line(format_args!("second"));
        for _ in 0..1000 {
            sink.write_line(format_args!("dropped"));
        }
        assert!(!sink.flush(Duration::from_secs(1)));
        assert!(matches!(receiver.try_recv(), Ok(Record::Line(line)) if line == "first\n"));
        assert!(matches!(receiver.try_recv(), Ok(Record::Line(line)) if line == "second\n"));
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn blocked_writer_cannot_hold_producer_or_flush_beyond_budget() {
        struct BlockedWriter(mpsc::SyncSender<()>, mpsc::Receiver<()>);
        impl Write for BlockedWriter {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                let _ = self.0.try_send(());
                self.1.recv().unwrap();
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let (entered, waiting) = mpsc::sync_channel(1);
        let (release, blocked) = mpsc::sync_channel(1);
        let (sender, receiver) = mpsc::sync_channel(2);
        let sink = Sink(sender);
        let worker =
            std::thread::spawn(move || write_records(BlockedWriter(entered, blocked), receiver));
        sink.write_line(format_args!("block"));
        waiting.recv_timeout(Duration::from_secs(1)).unwrap();
        let started = std::time::Instant::now();
        assert!(!sink.flush(Duration::from_millis(20)));
        assert!(started.elapsed() < Duration::from_secs(1));
        sink.write_line(format_args!("queued"));
        sink.write_line(format_args!("dropped"));
        drop(sink);
        release.send(()).unwrap();
        release.send(()).unwrap();
        worker.join().unwrap();
    }

    #[test]
    fn sink_errors_and_partial_writes_do_not_panic_or_retry_forever() {
        struct FailedWriter(Arc<Mutex<usize>>);
        impl Write for FailedWriter {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                let mut calls = self.0.lock().unwrap();
                *calls += 1;
                if *calls == 1 {
                    Ok(1)
                } else {
                    Err(io::ErrorKind::BrokenPipe.into())
                }
            }
            fn flush(&mut self) -> io::Result<()> {
                Err(io::ErrorKind::PermissionDenied.into())
            }
        }
        let calls = Arc::new(Mutex::new(0));
        let (sender, receiver) = mpsc::sync_channel(2);
        let (ack, received) = mpsc::sync_channel(1);
        sender.send(Record::Line("line\n".into())).unwrap();
        sender.send(Record::Flush(ack)).unwrap();
        drop(sender);
        write_records(FailedWriter(calls.clone()), receiver);
        assert_eq!(*calls.lock().unwrap(), 2);
        assert!(received.try_recv().is_ok());
    }
}
