use std::net::SocketAddr;

use clap::{Parser, Subcommand};
use tokio::sync::mpsc;

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
            let (tx, mut rx) = mpsc::channel::<core_engine::Observation>(1024);
            let printer = tokio::spawn(async move {
                while let Some(obs) = rx.recv().await {
                    let query_str = obs
                        .query
                        .as_deref()
                        .map_or(String::new(), |q| format!("?{q}"));
                    println!(
                        "[{}] {} {}{} -> {} ({}ms) [{:?}]",
                        obs.service_name,
                        obs.method,
                        obs.path,
                        query_str,
                        obs.status.as_u16(),
                        obs.duration.as_millis(),
                        obs.outcome
                    );
                }
            });

            let result =
                core_engine::proxy::run_with_observer(listen, target, name, Some(tx)).await;
            printer.abort();
            result?;
        }
    }

    Ok(())
}
