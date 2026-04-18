mod asset;
mod cli;
mod engine;
mod error;
mod introspect;
mod manifest;
mod precondition;
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
