//! Bounded, best-effort process diagnostics. Only the writer thread touches stderr.

use std::{
    fmt::{self, Write as _},
    io::{self, Write},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc::{self, SyncSender, TrySendError},
        Arc, OnceLock,
    },
    time::Duration,
};
use tokio::sync::oneshot;

const CAPACITY: usize = 256;
const MAX_MESSAGE_BYTES: usize = 1024;
static DIAGNOSTICS: OnceLock<Diagnostics> = OnceLock::new();

#[derive(Default)]
struct Counters {
    enqueued: AtomicU64,
    written: AtomicU64,
    full: AtomicU64,
    closed: AtomicU64,
    truncated: AtomicU64,
    write_errors: AtomicU64,
    spawn_errors: AtomicU64,
}

/// Independent atomic reads; consistent totals are available after producers stop.
#[derive(Debug, Clone, Copy)]
pub struct DiagnosticSnapshot {
    pub enqueued: u64,
    pub written: u64,
    pub dropped_full: u64,
    pub dropped_closed: u64,
    pub truncated: u64,
    pub write_errors: u64,
    pub spawn_errors: u64,
}

impl Counters {
    fn snapshot(&self) -> DiagnosticSnapshot {
        DiagnosticSnapshot {
            enqueued: self.enqueued.load(Ordering::Relaxed),
            written: self.written.load(Ordering::Relaxed),
            dropped_full: self.full.load(Ordering::Relaxed),
            dropped_closed: self.closed.load(Ordering::Relaxed),
            truncated: self.truncated.load(Ordering::Relaxed),
            write_errors: self.write_errors.load(Ordering::Relaxed),
            spawn_errors: self.spawn_errors.load(Ordering::Relaxed),
        }
    }
}

enum Command {
    Message(String),
    Finish(oneshot::Sender<io::Result<()>>),
}

struct Diagnostics {
    sender: SyncSender<Command>,
    counters: Arc<Counters>,
}

struct Message {
    text: String,
    truncated: bool,
}

impl fmt::Write for Message {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        let mut count = text.len().min(MAX_MESSAGE_BYTES - self.text.len());
        while !text.is_char_boundary(count) {
            count -= 1;
        }
        self.text.push_str(&text[..count]);
        if count < text.len() {
            self.truncated = true;
            return Err(fmt::Error);
        }
        Ok(())
    }
}

impl Diagnostics {
    fn start(mut writer: impl Write + Send + 'static, capacity: usize) -> Self {
        let (sender, receiver) = mpsc::sync_channel(capacity);
        let counters = Arc::new(Counters::default());
        let worker_counters = Arc::clone(&counters);
        let spawned = std::thread::Builder::new()
            .name("exaprop-diagnostics".into())
            .spawn(move || {
                while let Ok(command) = receiver.recv() {
                    match command {
                        Command::Message(message) => {
                            if writeln!(writer, "{message}").is_err() {
                                worker_counters.write_errors.fetch_add(1, Ordering::Relaxed);
                                return;
                            }
                            worker_counters.written.fetch_add(1, Ordering::Relaxed);
                        }
                        Command::Finish(done) => {
                            let stats = worker_counters.snapshot();
                            let result = writeln!(writer,
                                "diagnostics_queue enqueued={} written={} dropped_full={} dropped_closed={} truncated={} write_errors={} spawn_errors={} unconfirmed={}",
                                stats.enqueued, stats.written, stats.dropped_full, stats.dropped_closed,
                                stats.truncated, stats.write_errors, stats.spawn_errors,
                                stats.enqueued.saturating_sub(stats.written),
                            ).and_then(|()| writer.flush());
                            if result.is_err() {
                                worker_counters.write_errors.fetch_add(1, Ordering::Relaxed);
                            }
                            let _ = done.send(result);
                            return;
                        }
                    }
                }
            });
        if spawned.is_err() {
            counters.spawn_errors.fetch_add(1, Ordering::Relaxed);
        }
        Self { sender, counters }
    }

    fn emit(&self, arguments: fmt::Arguments<'_>) {
        let mut message = Message {
            text: String::with_capacity(MAX_MESSAGE_BYTES),
            truncated: false,
        };
        let _ = message.write_fmt(arguments);
        if message.truncated {
            self.counters.truncated.fetch_add(1, Ordering::Relaxed);
        }
        match self.sender.try_send(Command::Message(message.text)) {
            Ok(()) => {
                self.counters.enqueued.fetch_add(1, Ordering::Relaxed);
            }
            Err(TrySendError::Full(_)) => {
                self.counters.full.fetch_add(1, Ordering::Relaxed);
            }
            Err(TrySendError::Disconnected(_)) => {
                self.counters.closed.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    async fn finish(&self, wait: Duration) -> io::Result<()> {
        let finish = async {
            let (done, received) = oneshot::channel();
            let mut command = Command::Finish(done);
            loop {
                match self.sender.try_send(command) {
                    Ok(()) => break,
                    Err(TrySendError::Disconnected(_)) => {
                        return Err(io::Error::new(
                            io::ErrorKind::BrokenPipe,
                            "diagnostic writer unavailable",
                        ))
                    }
                    Err(TrySendError::Full(returned)) => {
                        command = returned;
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                }
            }
            received
                .await
                .map_err(|_| io::Error::other("diagnostic writer failed"))?
        };
        tokio::time::timeout(wait, finish).await.map_err(|_| {
            io::Error::new(
                io::ErrorKind::TimedOut,
                "diagnostics did not drain within the shutdown deadline",
            )
        })?
    }
}

/// Attempt a bounded diagnostic send. Never waits for a sink or channel capacity.
pub fn emit(arguments: fmt::Arguments<'_>) {
    DIAGNOSTICS
        .get_or_init(|| Diagnostics::start(io::stderr(), CAPACITY))
        .emit(arguments);
}

/// Read process-wide counters without writing to the logging destination.
pub fn snapshot() -> DiagnosticSnapshot {
    DIAGNOSTICS.get().map_or_else(
        || Counters::default().snapshot(),
        |diagnostics| diagnostics.counters.snapshot(),
    )
}

/// CLI process-exit drain. Call only after diagnostic producers have stopped.
/// After successful completion this process-wide sink is closed permanently.
/// A blocked destination times out; the dedicated writer does not keep Tokio alive.
pub async fn finish(wait: Duration) -> io::Result<()> {
    if let Some(diagnostics) = DIAGNOSTICS.get() {
        diagnostics.finish(wait).await
    } else {
        Ok(())
    }
}

#[macro_export]
macro_rules! diagnostic {
    ($($argument:tt)*) => { $crate::diagnostics::emit(format_args!($($argument)*)) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn blocked_writer_drops_at_capacity_without_blocking_producer() {
        struct Gate {
            started: Option<mpsc::Sender<()>>,
            release: mpsc::Receiver<()>,
        }
        impl Write for Gate {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                if let Some(started) = self.started.take() {
                    started.send(()).unwrap();
                    self.release.recv_timeout(Duration::from_secs(3)).unwrap();
                }
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let (started, ready) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let logger = Diagnostics::start(
            Gate {
                started: Some(started),
                release: released,
            },
            1,
        );
        logger.emit(format_args!("one"));
        ready.recv_timeout(Duration::from_secs(2)).unwrap();
        logger.emit(format_args!("two"));
        logger.emit(format_args!("three"));
        let stats = logger.counters.snapshot();
        assert_eq!(stats.enqueued, 2);
        assert_eq!(stats.dropped_full, 1);
        release.send(()).unwrap();
        logger.finish(Duration::from_secs(2)).await.unwrap();
        assert_eq!(logger.counters.snapshot().written, 2);
    }

    #[tokio::test]
    async fn broken_writer_counts_failure_and_closed_sends() {
        struct Broken;
        impl Write for Broken {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let logger = Diagnostics::start(Broken, 1);
        logger.emit(format_args!("one"));
        tokio::time::timeout(Duration::from_secs(2), async {
            while logger.counters.snapshot().write_errors == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        // Wait for the receiver to be dropped, without depending on scheduling.
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                logger.emit(format_args!("after failure"));
                if logger.counters.snapshot().dropped_closed > 0 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(logger.finish(Duration::from_secs(1)).await.is_err());
        assert_eq!(logger.counters.snapshot().written, 0);
    }

    #[test]
    fn rendered_messages_are_bounded_and_valid_utf8() {
        let mut message = Message {
            text: String::with_capacity(MAX_MESSAGE_BYTES),
            truncated: false,
        };
        let _ = message.write_fmt(format_args!("{}", "é".repeat(MAX_MESSAGE_BYTES)));
        assert!(message.truncated);
        assert!(message.text.len() <= MAX_MESSAGE_BYTES);
        assert!(message.text.is_char_boundary(message.text.len()));
    }
}
