//! `choosh-relayd`: entry point only. See `lib.rs` for the implementation
//! and `docs/specs/relay-protocol.md` / `docs/specs/auth-and-enrollment.md`
//! for the protocol this binary implements.
//!
//! Also dispatches the `pair` subcommand (`choosh-relayd pair`), which
//! prints the currently-configured `CHOOSH_RELAYD_BOOTSTRAP_SECRET` as a
//! scannable QR code so a phone can complete `WebAuthn` registration
//! without the secret ever being typed by hand or baked into the
//! distributed APK — see `choosh_relayd::pair` for the rendering logic.
//! With no arguments (the deployed systemd unit's `ExecStart=`), behavior
//! is unchanged: it starts the server via `choosh_relayd::run()`.

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if std::env::args().nth(1).as_deref() == Some("pair") {
        return pair();
    }
    choosh_relayd::run().await
}

/// Implements the `pair` subcommand: reads the already-configured
/// `CHOOSH_RELAYD_BOOTSTRAP_SECRET` from the environment and prints it as a
/// pairing QR code. Never mints a new secret — this command's job is to
/// display the value the running server actually checks.
fn pair() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let secret = std::env::var("CHOOSH_RELAYD_BOOTSTRAP_SECRET").unwrap_or_default();
    if secret.is_empty() {
        eprintln!(
            "error: CHOOSH_RELAYD_BOOTSTRAP_SECRET is not set (or is empty).\n\
             \n\
             `choosh-relayd pair` displays the bootstrap secret the running\n\
             relayd server was started with, so a phone can scan it and\n\
             complete WebAuthn registration — it does not generate a new\n\
             secret, since one minted here would not match what the server\n\
             actually checks.\n\
             \n\
             For local testing, generate one and export it before starting\n\
             both the server and this command:\n\
             \n\
             \x20 export CHOOSH_RELAYD_BOOTSTRAP_SECRET=$(openssl rand -hex 32)\n\
             \n\
             In a real deployment, this should already be set in the\n\
             systemd unit's Environment=/EnvironmentFile= — set it there so\n\
             the running server and `choosh-relayd pair` agree on the same\n\
             value."
        );
        std::process::exit(1);
    }
    print!("{}", choosh_relayd::pair::render_pairing_qr(&secret)?);
    Ok(())
}
