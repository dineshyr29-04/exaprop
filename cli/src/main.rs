use std::net::SocketAddr;

use clap::{Parser, Subcommand};

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
    },
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    match Args::parse().command {
        Command::Attach { listen, target } => core_engine::proxy::run(listen, target).await?,
    }

    Ok(())
}
