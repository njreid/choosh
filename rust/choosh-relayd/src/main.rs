//! `choosh-relayd`: entry point only. See `lib.rs` for the implementation
//! and `docs/specs/relay-protocol.md` / `docs/specs/auth-and-enrollment.md`
//! for the protocol this binary implements.

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    choosh_relayd::run().await
}
