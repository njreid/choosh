//! `choosh-hostd`: entry point only. See `lib.rs` for the implementation
//! and `docs/specs/relay-protocol.md` / `docs/specs/auth-and-enrollment.md`
//! / `docs/specs/host-deployment.md` for the behavior this binary
//! implements.

use clap::Parser;
use choosh_hostd::Cli;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    choosh_hostd::run(Cli::parse()).await
}
