mod asset;
mod bridge;
mod cli;
mod contract;
mod engine;
mod error;
mod ingress_meta;
mod introspect;
mod manifest;
mod operator;
mod precondition;
mod registry;
mod runner;
mod state;

use clap::Parser;

fn main() {
    let cli = cli::Cli::parse();

    if let Err(e) = cli::dispatch(cli) {
        eprintln!("error: {}", e);
        std::process::exit(1);
    }
}
