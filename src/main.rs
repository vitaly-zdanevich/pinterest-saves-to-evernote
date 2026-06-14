mod config;
mod evernote_client;
mod image;
mod note;
mod pinterest;
mod state;
mod sync;

use anyhow::Result;
use clap::{Parser, Subcommand};

use crate::config::Settings;

#[derive(Debug, Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Export newly saved Pinterest pins to Evernote.
    Sync,
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cli = Cli::parse();
    match cli.command {
        Command::Sync => sync::run(Settings::from_env()?).await,
    }
}
