//! Keyless production Machine process.

#![forbid(unsafe_code)]

use std::path::PathBuf;

use anyhow::{Context, Result};
use bloom_triad_protocol::{
    Empty, MachineBrokerRequest, MachineBrokerResponse, ProtocolError, ProtocolErrorCode,
};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "bloom-machine", version)]
struct Cli {
    #[arg(
        long,
        env = "BLOOM_BROKER_SOCKET",
        default_value = "/var/run/bloom/broker.sock"
    )]
    broker_socket: PathBuf,
    #[arg(
        long,
        env = "BLOOM_MACHINE_IDENTITY",
        default_value = "/var/run/bloom/machine-identity.json"
    )]
    identity: PathBuf,
    #[arg(
        long,
        env = "BLOOM_EDGE_MANIFEST",
        default_value = "/etc/bloom/edge-manifest.json"
    )]
    edge_manifest: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Query Broker readiness through the authenticated Machine edge.
    Readiness,
    /// Query the exact compiled Broker capability set.
    Capabilities,
    /// Dispatch one closed-schema Machine→Broker request from stdin.
    Request,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let broker = bloom_machine_client::MachineBrokerClient::connect_unix_from_files(
        cli.broker_socket,
        cli.identity,
        cli.edge_manifest,
    )
    .context("load authenticated Machine-to-Broker edge")?;

    let request = match cli.command {
        Command::Readiness => MachineBrokerRequest::BrokerReadiness(Empty {}),
        Command::Capabilities => MachineBrokerRequest::BrokerCapabilities(Empty {}),
        Command::Request => serde_json::from_reader(std::io::stdin().lock())
            .context("decode closed Machine-to-Broker request from stdin")?,
    };
    let response = broker.request(request.clone()).await?;
    require_matching_method(&request, &response)?;
    serde_json::to_writer(std::io::stdout().lock(), &response).context("write Broker response")?;
    println!();
    Ok(())
}

fn require_matching_method(
    request: &MachineBrokerRequest,
    response: &MachineBrokerResponse,
) -> Result<(), ProtocolError> {
    let request_method = serde_json::to_value(request).ok().and_then(|value| {
        value
            .get("method")
            .and_then(|method| method.as_str())
            .map(str::to_owned)
    });
    let response_method = serde_json::to_value(response).ok().and_then(|value| {
        value
            .get("method")
            .and_then(|method| method.as_str())
            .map(str::to_owned)
    });
    if request_method != response_method {
        return Err(ProtocolError::new(
            ProtocolErrorCode::MalformedFrame,
            "Broker response method does not match the Machine request",
        ));
    }
    Ok(())
}
