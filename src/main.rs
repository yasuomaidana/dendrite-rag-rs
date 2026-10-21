use std::io;

use clap::{Parser, Subcommand};
use dendrite_rag::rpc::{self, Handler, RpcError};
use serde_json::Value;

/// Retrieval engine for source-code repositories.
#[derive(Parser)]
#[command(name = "dendrite-rag", version)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Serve JSON-RPC 2.0 over stdio (the default).
    Serve,
}

struct Server;

impl Handler for Server {
    fn call(&mut self, method: &str, _params: Value) -> Result<Value, RpcError> {
        match method {
            "health" => Ok(rpc::health()),
            _ => Err(RpcError::new(
                rpc::code::METHOD_NOT_FOUND,
                format!("unknown method {method}"),
            )),
        }
    }
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command.unwrap_or(Command::Serve) {
        Command::Serve => rpc::serve(&mut Server, io::stdin().lock(), io::stdout().lock())?,
    }
    Ok(())
}
