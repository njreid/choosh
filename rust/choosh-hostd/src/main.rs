// See `lib.rs`'s matching attribute for why this is needed (`jj_ops.rs`'s
// real `jj-lib` calls push this crate's deepest async future nesting past
// rustc's default query-depth recursion limit) — this binary crate root
// needs its own copy since `#![recursion_limit]` doesn't cross a crate
// boundary, even a `lib`-to-its-own-`bin` one.
#![recursion_limit = "512"]
//! `choosh-hostd`: entry point only. See `lib.rs` for the implementation
//! and `docs/specs/relay-protocol.md` / `docs/specs/auth-and-enrollment.md`
//! / `docs/specs/host-deployment.md` for the behavior this binary
//! implements.

use clap::Parser;
use choosh_hostd::Cli;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    Box::pin(choosh_hostd::run(Cli::parse())).await
}
