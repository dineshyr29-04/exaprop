use std::{
    io::{self, Write},
    net::SocketAddr,
    time::{Duration, UNIX_EPOCH},
};

use clap::{Parser, Subcommand};
use tokio::sync::{mpsc, oneshot};

const OBSERVATION_CAPACITY: usize = 1024;
const OUTPUT_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);

fn print_observations(
    mut receiver: mpsc::Receiver<core_engine::Observation>,
    mut writer: impl Write,
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
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    match Args::parse().command {
        Command::Attach {
            listen,
            target,
            name,
        } => {
            let (tx, rx) = mpsc::channel(OBSERVATION_CAPACITY);
            let (done_tx, done_rx) = oneshot::channel();
            // A dedicated thread keeps blocking stdout writes off Tokio workers.
            // Unlike spawn_blocking, it does not hold runtime shutdown open if
            // an output pipe stops accepting data indefinitely.
            std::thread::Builder::new()
                .name("exaprop-observations".into())
                .spawn(move || {
                    let stdout = io::stdout();
                    let result = print_observations(rx, stdout.lock());
                    let _ = done_tx.send(result);
                })?;

            let result =
                core_engine::proxy::run_with_observer(listen, target, name, Some(tx)).await;
            match tokio::time::timeout(OUTPUT_SHUTDOWN_TIMEOUT, done_rx).await {
                Ok(Ok(Ok(()))) => {}
                Ok(Ok(Err(error))) => eprintln!("Observation output failed: {error}"),
                Ok(Err(error)) => eprintln!("Observation output thread failed: {error}"),
                Err(_) => eprintln!(
                    "Observation output did not drain within 2 seconds; queued records may be lost"
                ),
            }
            result?;
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
        print_observations(rx, &mut output).unwrap();
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
            print_observations(rx, BrokenWriter).unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
    }
}
