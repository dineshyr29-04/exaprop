pub mod reporting;

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

use core_engine::detection::{DetectionConfig, Detector};
use reporting::{CompletionState, DetectionStats, TelemetryPipeline};

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

        /// Enable deterministic anomaly detection and bounded V1 telemetry output.
        #[arg(long)]
        detect: bool,

        /// Duration threshold in milliseconds (requires --detect; default 1000).
        #[arg(long, requires = "detect", value_parser = clap::value_parser!(u64).range(1..=3_600_000))]
        latency_threshold_ms: Option<u64>,
    },
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let args = match Args::try_parse() {
        Ok(args) => args,
        Err(error) => {
            // Clap's default error/exit path writes synchronously. Keep sink I/O
            // on dedicated threads even before the listener starts.
            if error.use_stderr() {
                emit_cli_error(&error.to_string());
                let _ = core_engine::diagnostics::finish(DIAGNOSTIC_SHUTDOWN_TIMEOUT).await;
                return std::process::ExitCode::from(2);
            }
            return if print_help(error.to_string()).await.is_ok() {
                std::process::ExitCode::SUCCESS
            } else {
                std::process::ExitCode::FAILURE
            };
        }
    };
    let Command::Attach {
        detect,
        latency_threshold_ms,
        ref name,
        ..
    } = args.command;
    let config = match validate_attach_args(detect, latency_threshold_ms, name) {
        Ok(config) => config,
        Err(error) => {
            emit_cli_error(error);
            let _ = core_engine::diagnostics::finish(DIAGNOSTIC_SHUTDOWN_TIMEOUT).await;
            return std::process::ExitCode::from(2);
        }
    };
    let result = run(args.command, config).await;
    if let Err(error) = &result {
        let (escaped, _) = reporting::escape_and_bound(&error.to_string(), 900);
        core_engine::diagnostic!("Error: {escaped}");
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

fn emit_cli_error(message: &str) {
    let (escaped, truncated) = reporting::escape_and_bound(message, 900);
    core_engine::diagnostic!(
        "CLI error: {}{}; use exaprop attach --help",
        escaped,
        if truncated { " [truncated]" } else { "" }
    );
}

async fn print_help(text: String) -> io::Result<()> {
    let (done_tx, done_rx) = oneshot::channel();
    std::thread::Builder::new()
        .name("exaprop-help".into())
        .spawn(move || {
            let mut stdout = io::stdout();
            let result = stdout
                .write_all(text.as_bytes())
                .and_then(|()| stdout.flush());
            let _ = done_tx.send(result);
        })?;
    tokio::time::timeout(OUTPUT_SHUTDOWN_TIMEOUT, done_rx)
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "help output timed out"))?
        .map_err(|_| io::Error::other("help output worker failed"))?
}

fn validate_attach_args(
    detect: bool,
    threshold: Option<u64>,
    name: &str,
) -> Result<Option<DetectionConfig>, &'static str> {
    if !detect {
        return if threshold.is_some() {
            Err("--latency-threshold-ms requires --detect")
        } else {
            Ok(None)
        };
    }
    // Encoded bytes, not character count. Escaping stops at the first overflow.
    let (_, truncated) = reporting::escape_and_bound(name, reporting::SERVICE_ENCODED_BUDGET);
    if truncated || name.trim().is_empty() || name.chars().any(char::is_control) {
        return Err("--detect requires --name to be nonblank, free of controls, and at most 128 escaped bytes; use a nonsecret label");
    }
    DetectionConfig::new(threshold.unwrap_or(DetectionConfig::DEFAULT_THRESHOLD_MS))
        .map(Some)
        .map_err(|_| "--latency-threshold-ms must be an integer from 1 to 3600000")
}

async fn run(
    command: Command,
    config: Option<DetectionConfig>,
) -> Result<(), Box<dyn std::error::Error>> {
    match command {
        Command::Attach {
            listen,
            target,
            name,
            ..
        } => {
            if !listen.ip().is_loopback() {
                core_engine::diagnostic!("Warning: non-loopback listener exposes plain HTTP; ExaProp adds no TLS, authentication, or access control");
            }
            if let Some(config) = config {
                return run_detection(listen, target, name, config).await;
            }
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

async fn run_detection(
    listen: SocketAddr,
    target: SocketAddr,
    name: String,
    config: DetectionConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    let input = core_engine::ObservationStats::default();
    let stats = DetectionStats::new_shared();
    // Retain stats independently: shutdown consumes the pipeline even on failure.
    let (pipeline, tx) = TelemetryPipeline::spawn(
        OBSERVATION_CAPACITY,
        Detector::new(config),
        io::stdout(),
        Arc::clone(&stats),
    )?;
    // Move the only external sender into the proxy. Its handlers release their
    // clones before this future completes, including after a listener bind error.
    let proxy_result = core_engine::proxy::run_with_observer_and_stats(
        listen,
        target,
        name,
        Some(tx),
        input.clone(),
    )
    .await;
    let output_result = finish_detection(
        pipeline,
        &stats,
        &input,
        reporting::TELEMETRY_SHUTDOWN_TIMEOUT,
    )
    .await;
    // Complete teardown and summaries even when proxy startup or shutdown failed.
    proxy_result?;
    output_result?;
    Ok(())
}

async fn finish_detection(
    pipeline: TelemetryPipeline,
    stats: &DetectionStats,
    input: &core_engine::ObservationStats,
    timeout: Duration,
) -> io::Result<()> {
    let result = pipeline.shutdown(timeout).await;
    let input = input.snapshot();
    let (snapshot, result) = match result {
        Ok(snapshot) => {
            let result =
                reporting::check_detection_settled(&snapshot, input).map_err(io::Error::other);
            (snapshot, result)
        }
        Err(error) => (stats.snapshot(), Err(error)),
    };
    let state = CompletionState::from_result(&result);
    let detection = reporting::format_detection_stats_diagnostic(&snapshot)
        .map_err(|_| io::Error::other("detection summary exceeded byte budget"))?;
    let output = reporting::format_anomaly_output_diagnostic(&snapshot, input, state)
        .map_err(|_| io::Error::other("output summary exceeded byte budget"))?;
    core_engine::diagnostic!("{detection}");
    core_engine::diagnostic!("{output}");
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn detection_flags_validate_defaults_boundaries_and_dependency() {
        for threshold in [1, 1000, 3_600_000] {
            let value = threshold.to_string();
            let args = Args::try_parse_from([
                "exaprop",
                "attach",
                "--detect",
                "--latency-threshold-ms",
                &value,
            ])
            .unwrap();
            let Command::Attach {
                detect,
                latency_threshold_ms,
                name,
                ..
            } = args.command;
            let config = validate_attach_args(detect, latency_threshold_ms, &name)
                .unwrap()
                .unwrap();
            assert_eq!(config.latency_threshold(), Duration::from_millis(threshold));
        }
        assert_eq!(
            validate_attach_args(true, None, "orders").unwrap().unwrap(),
            DetectionConfig::default()
        );
        assert_eq!(validate_attach_args(false, None, "").unwrap(), None);
        assert!(validate_attach_args(false, Some(1), "orders").is_err());
        for value in ["0", "3600001", "-1", "1.5", "18446744073709551616", "text"] {
            let error = Args::try_parse_from([
                "exaprop",
                "attach",
                "--detect",
                "--latency-threshold-ms",
                value,
            ])
            .unwrap_err();
            assert_eq!(error.exit_code(), 2);
        }
        for flags in [
            vec!["--latency-threshold-ms", "500"],
            vec![
                "--detect",
                "--latency-threshold-ms",
                "500",
                "--latency-threshold-ms",
                "600",
            ],
            vec!["--detect", "--detect"],
        ] {
            assert_eq!(
                Args::try_parse_from(["exaprop", "attach"].into_iter().chain(flags))
                    .unwrap_err()
                    .exit_code(),
                2
            );
        }
    }

    #[test]
    fn service_policy_applies_only_to_detection_and_uses_encoded_bytes() {
        for name in [
            "",
            "  ",
            "bad\nname",
            "bad\x1bname",
            &"a".repeat(129),
            &"🦀".repeat(9),
        ] {
            assert!(validate_attach_args(true, None, name).is_err());
            assert!(validate_attach_args(false, None, name).unwrap().is_none());
        }
        for name in ["orders", &"a".repeat(128), &"🦀".repeat(8)] {
            assert!(validate_attach_args(true, None, name).is_ok());
        }
        for threshold in [0, 3_600_001, u64::MAX] {
            assert!(validate_attach_args(true, Some(threshold), "orders").is_err());
        }
    }

    #[tokio::test]
    async fn failed_pipeline_retains_counters_and_returns_failure() {
        struct Broken;
        impl Write for Broken {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let stats = DetectionStats::new_shared();
        let (pipeline, tx) = TelemetryPipeline::spawn(
            1,
            Detector::new(DetectionConfig::default()),
            Broken,
            Arc::clone(&stats),
        )
        .unwrap();
        tx.try_send(core_engine::Observation::new(
            UNIX_EPOCH,
            "orders".into(),
            core_engine::Method::GET,
            "/",
            core_engine::StatusCode::INTERNAL_SERVER_ERROR,
            Duration::ZERO,
            core_engine::RequestOutcome::Success,
        ))
        .unwrap();
        drop(tx);
        let result = finish_detection(
            pipeline,
            &stats,
            &core_engine::ObservationStats::default(),
            Duration::from_secs(2),
        )
        .await;
        assert_eq!(
            CompletionState::from_result(&result),
            CompletionState::Failed
        );
        assert_eq!(stats.snapshot().anomalous, 1);
        assert_eq!(stats.snapshot().write_errors, 1);
        assert_eq!(stats.snapshot().events_unconfirmed(), 1);
    }

    #[tokio::test]
    async fn invariant_failure_returns_error_and_live_producer_times_out() {
        let input = core_engine::ObservationStats::default();
        let stats = DetectionStats::new_shared();
        let (pipeline, tx) = TelemetryPipeline::spawn(
            1,
            Detector::new(DetectionConfig::default()),
            io::sink(),
            Arc::clone(&stats),
        )
        .unwrap();
        tx.try_send(core_engine::Observation::new(
            UNIX_EPOCH,
            "orders".into(),
            core_engine::Method::GET,
            "/",
            core_engine::StatusCode::OK,
            Duration::ZERO,
            core_engine::RequestOutcome::Success,
        ))
        .unwrap();
        drop(tx);
        // Synthetic sends bypass proxy input accounting: successful workers must
        // still fail settlement rather than panic or report clean coverage.
        let result = finish_detection(pipeline, &stats, &input, Duration::from_secs(2)).await;
        assert_eq!(
            CompletionState::from_result(&result),
            CompletionState::Failed
        );
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("input enqueued != evaluated"));
        let (pipeline, tx) = TelemetryPipeline::spawn(
            1,
            Detector::new(DetectionConfig::default()),
            io::sink(),
            Arc::clone(&stats),
        )
        .unwrap();
        let result = finish_detection(pipeline, &stats, &input, Duration::from_millis(10)).await;
        assert_eq!(
            CompletionState::from_result(&result),
            CompletionState::TimedOut
        );
        drop(tx); // release the deliberate retained producer so workers can exit
    }

    #[test]
    fn parses_service_name_and_default_addresses() {
        let args = Args::try_parse_from(["exaprop", "attach", "--name", "orders"]).unwrap();
        let Command::Attach {
            listen,
            target,
            name,
            ..
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
