use std::{
    io::{self, Write},
    net::SocketAddr,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, UNIX_EPOCH},
};

use clap::{Parser, Subcommand};
use tokio::sync::{mpsc, oneshot};

const OBSERVATION_CAPACITY: usize = 1024;
const OUTPUT_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);
const DIAGNOSTIC_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);

fn print_observations(
    mut receiver: mpsc::Receiver<core_engine::Observation>,
    mut writer: impl Write,
    written: &AtomicU64,
) -> io::Result<()> {
    while let Some(obs) = receiver.blocking_recv() {
        let query = obs
            .query
            .as_deref()
            .map_or(String::new(), |q| format!("?{q}"));
        let timestamp = obs
            .timestamp
            .duration_since(UNIX_EPOCH)
            .map_err(io::Error::other)?;
        writeln!(
            writer,
            "[{}] {} {}{} -> {} ({:.3}ms) [{:?}] timestamp={}.{:09}",
            obs.service_name.escape_default(),
            obs.method,
            obs.path.escape_default(),
            query.escape_default(),
            obs.status.as_u16(),
            obs.duration.as_secs_f64() * 1000.0,
            obs.outcome,
            timestamp.as_secs(),
            timestamp.subsec_nanos(),
        )?;
        written.fetch_add(1, Ordering::Relaxed);
    }
    writer.flush()
}

/// ExaProp's local HTTP/1.1 reverse proxy.
#[derive(Debug, Parser)]
#[command(version, about)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Forward HTTP requests to one backend and relay its responses.
    Attach {
        /// Local address where clients connect.
        #[arg(long, default_value = "127.0.0.1:8080")]
        listen: SocketAddr,

        /// HTTP backend address.
        #[arg(long, default_value = "127.0.0.1:8081")]
        target: SocketAddr,

        /// Service identifier for observation tagging.
        #[arg(long, default_value = "backend-service")]
        name: String,
    },
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let result = run().await;
    if let Err(error) = &result {
        core_engine::diagnostic!("Error: {error}");
    }
    // Returning Result would let Rust's termination handler synchronously write
    // to stderr. Use ExitCode so even an error with a blocked sink can exit.
    let diagnostics = core_engine::diagnostics::finish(DIAGNOSTIC_SHUTDOWN_TIMEOUT).await;
    if result.is_err() || diagnostics.is_err() {
        std::process::ExitCode::FAILURE
    } else {
        std::process::ExitCode::SUCCESS
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    match Args::parse().command {
        Command::Attach {
            listen,
            target,
            name,
        } => {
            let (tx, rx) = mpsc::channel(OBSERVATION_CAPACITY);
            let (done_tx, done_rx) = oneshot::channel();
            let stats = core_engine::ObservationStats::default();
            let written = Arc::new(AtomicU64::new(0));
            let printer_written = Arc::clone(&written);
            // A dedicated thread keeps blocking stdout writes off Tokio workers.
            // Unlike spawn_blocking, it does not hold runtime shutdown open if
            // an output pipe stops accepting data indefinitely.
            std::thread::Builder::new()
                .name("exaprop-observations".into())
                .spawn(move || {
                    let stdout = io::stdout();
                    let result = print_observations(rx, stdout.lock(), &printer_written);
                    let _ = done_tx.send(result);
                })?;

            let result = core_engine::proxy::run_with_observer_and_stats(
                listen,
                target,
                name,
                Some(tx),
                stats.clone(),
            )
            .await;
            let output_result = match tokio::time::timeout(OUTPUT_SHUTDOWN_TIMEOUT, done_rx).await {
                Ok(Ok(result)) => result.map_err(|error| {
                    io::Error::new(error.kind(), format!("Observation output failed: {error}"))
                }),
                Ok(Err(error)) => Err(io::Error::other(format!(
                    "Observation output thread failed: {error}"
                ))),
                Err(_) => Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "Observation output did not drain within 2 seconds; queued records may be lost",
                )),
            };
            let written = written.load(Ordering::Relaxed);
            core_engine::diagnostic!(
                "observation_output written={} unconfirmed={} drained={}",
                written,
                stats.snapshot().enqueued.saturating_sub(written),
                output_result.is_ok()
            );
            result?;
            output_result?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn parses_service_name_and_default_addresses() {
        let args = Args::try_parse_from(["exaprop", "attach", "--name", "orders"]).unwrap();
        let Command::Attach {
            listen,
            target,
            name,
        } = args.command;
        assert_eq!(listen.to_string(), "127.0.0.1:8080");
        assert_eq!(target.to_string(), "127.0.0.1:8081");
        assert_eq!(name, "orders");
        assert!(Args::try_parse_from(["exaprop", "attach", "--target", "invalid"]).is_err());
    }

    #[test]
    fn drains_queued_records_and_escapes_terminal_controls() {
        let (tx, rx) = mpsc::channel(2);
        let mut observation = core_engine::Observation::new(
            UNIX_EPOCH + Duration::from_secs(42),
            "svc\n\u{1b}[31m".into(),
            "GET".parse().unwrap(),
            "/health?token=secret",
            200u16.try_into().unwrap(),
            Duration::from_micros(125),
            core_engine::RequestOutcome::Success,
        );
        tx.try_send(observation.clone()).unwrap();
        observation.path = "/second".into();
        tx.try_send(observation).unwrap();
        drop(tx);
        let mut output = Vec::new();
        let written = AtomicU64::new(0);
        print_observations(rx, &mut output, &written).unwrap();
        assert_eq!(written.load(Ordering::Relaxed), 2);
        let output = String::from_utf8(output).unwrap();
        assert_eq!(output.lines().count(), 2);
        assert!(!output.contains('\u{1b}'));
        assert!(!output.contains("secret"));
        assert!(output.contains("svc\\n\\u{1b}[31m"));
        assert!(output.contains("0.125ms"));
        assert!(output.contains("timestamp=42.000000000"));
        assert!(output.contains("/second"));
    }

    #[test]
    fn reports_output_errors_without_panicking() {
        struct BrokenWriter;
        impl Write for BrokenWriter {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::from(io::ErrorKind::BrokenPipe))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let (tx, rx) = mpsc::channel(1);
        tx.try_send(core_engine::Observation::new(
            UNIX_EPOCH,
            "svc".into(),
            "GET".parse().unwrap(),
            "/",
            200u16.try_into().unwrap(),
            Duration::ZERO,
            core_engine::RequestOutcome::Success,
        ))
        .unwrap();
        drop(tx);
        assert_eq!(
            print_observations(rx, BrokenWriter, &AtomicU64::new(0))
                .unwrap_err()
                .kind(),
            io::ErrorKind::BrokenPipe
        );
    }
}
