//! Ad-hoc dev/test tool — NOT shipped anywhere near the Android app or a
//! release build. This is the same "drive the real protocol code directly"
//! idea as `dev_passkey.rs` (which it reuses), applied one layer up: rather
//! than automating the Android emulator's UI for every RPC needed to stand
//! up and exercise a real end-to-end devhost/agent test (the app has no UI
//! yet for `request-enrollment-token`/`workspace.create`/`item.create` —
//! real, tracked gaps, not this tool's problem to solve), this talks to a
//! real `relayd` using the exact same `choosh-android-transport::PhoneConnection`
//! and `choosh-protocol::host_rpc` types the real app would use. Every
//! response this tool gets is a genuine `relayd`/`choosh-hostd` response,
//! not a simulation.
//!
//! Requires `--features dev-passkey` (for the software `WebAuthn`
//! authenticator) to build at all.
//!
//! Usage:
//!   dev_cli enroll-token   <http_base_url> <ws_url> <session_file>
//!   dev_cli list-devhosts  <ws_url> <session_file>
//!   dev_cli create-workspace <ws_url> <session_file> <devhost_id> <workspace_name> <clone_url>
//!   dev_cli create-item    <ws_url> <session_file> <workspace_id> <devhost_id> <name> <agent>

use choosh_android_bridge::dev_passkey;
use choosh_android_transport::PhoneConnection;
use choosh_protocol::host_rpc::{AgentKind, ItemType, ProjectSource, RpcRequest, RpcResponse};
use choosh_protocol::relay::IdentityClass;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let Some(command) = args.get(1) else {
        eprintln!("usage: dev_cli <enroll-token|list-devhosts|create-workspace|create-item> ...");
        std::process::exit(2);
    };

    let result = match command.as_str() {
        "enroll-token" => enroll_token(&args[2..]).await,
        "mint-token" => mint_token(&args[2..]).await,
        "list-devhosts" => list_devhosts(&args[2..]).await,
        "listen" => listen(&args[2..]).await,
        "create-workspace" => create_workspace(&args[2..]).await,
        "create-item" => create_item(&args[2..]).await,
        other => Err(format!("unknown command: {other}")),
    };

    if let Err(error) = result {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

async fn enroll_token(args: &[String]) -> Result<(), String> {
    let [http_base_url, ws_url, session_file] = args else {
        return Err("usage: enroll-token <http_base_url> <ws_url> <session_file>".to_string());
    };

    let client = reqwest::Client::new();
    let creation_options_json = client
        .post(format!("{http_base_url}/webauthn/register/start"))
        .send()
        .await
        .map_err(|error| format!("register/start request failed: {error}"))?
        .text()
        .await
        .map_err(|error| format!("register/start body read failed: {error}"))?;
    eprintln!("register/start -> {creation_options_json}");

    let credential_json = dev_passkey::register(http_base_url, &creation_options_json)?;
    eprintln!("dev passkey produced a credential ({} bytes)", credential_json.len());

    let finish_body = client
        .post(format!("{http_base_url}/webauthn/register/finish"))
        .header("Content-Type", "application/json")
        .body(credential_json)
        .send()
        .await
        .map_err(|error| format!("register/finish request failed: {error}"))?
        .text()
        .await
        .map_err(|error| format!("register/finish body read failed: {error}"))?;
    eprintln!("register/finish -> {finish_body}");

    let parsed: serde_json::Value =
        serde_json::from_str(&finish_body).map_err(|error| format!("register/finish response was not JSON: {error}"))?;
    let session_credential = parsed["session_credential"]
        .as_str()
        .ok_or_else(|| format!("register/finish response had no session_credential: {finish_body}"))?;

    std::fs::write(session_file, session_credential).map_err(|error| format!("failed to write {session_file}: {error}"))?;
    eprintln!("session credential saved to {session_file}");

    let mut connection = PhoneConnection::connect(ws_url, session_credential)
        .await
        .map_err(|error| format!("connect failed: {error}"))?;
    let (token, expires_at) = connection
        .request_enrollment_token(IdentityClass::Devhost)
        .await
        .map_err(|error| format!("request_enrollment_token failed: {error}"))?;
    println!("ENROLLMENT_TOKEN={token}");
    println!("EXPIRES_AT={expires_at}");
    Ok(())
}

async fn connect_from_session_file(ws_url: &str, session_file: &str) -> Result<PhoneConnection, String> {
    let session_credential =
        std::fs::read_to_string(session_file).map_err(|error| format!("failed to read {session_file}: {error}"))?;
    PhoneConnection::connect(ws_url, session_credential.trim()).await.map_err(|error| format!("connect failed: {error}"))
}

/// [`enroll_token`] without the register-a-new-passkey step — reuses an
/// already-saved session credential (from an earlier `enroll-token` run) to
/// mint another enrollment token, e.g. because the first one expired
/// before it got used.
async fn mint_token(args: &[String]) -> Result<(), String> {
    let [ws_url, session_file] = args else {
        return Err("usage: mint-token <ws_url> <session_file>".to_string());
    };
    let mut connection = connect_from_session_file(ws_url, session_file).await?;
    let (token, expires_at) = connection
        .request_enrollment_token(IdentityClass::Devhost)
        .await
        .map_err(|error| format!("request_enrollment_token failed: {error}"))?;
    println!("ENROLLMENT_TOKEN={token}");
    println!("EXPIRES_AT={expires_at}");
    Ok(())
}

/// Connects and prints every live `ServerPush::AgentEvent` received for
/// `seconds` — direct, real proof of what actually reaches the phone side,
/// not an inference from server logs.
async fn listen(args: &[String]) -> Result<(), String> {
    let [ws_url, session_file, seconds] = args else {
        return Err("usage: listen <ws_url> <session_file> <seconds>".to_string());
    };
    let seconds: u64 = seconds.parse().map_err(|_| "seconds must be a number".to_string())?;
    let mut connection = connect_from_session_file(ws_url, session_file).await?;
    let mut receiver = connection.take_agent_event_receiver().ok_or("no agent-event receiver available")?;
    eprintln!("listening for {seconds}s...");
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(seconds);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, receiver.recv()).await {
            Ok(Some(push)) => println!("{push:?}"),
            Ok(None) => {
                eprintln!("connection closed");
                break;
            }
            Err(_) => break,
        }
    }
    Ok(())
}

async fn list_devhosts(args: &[String]) -> Result<(), String> {
    let [ws_url, session_file] = args else {
        return Err("usage: list-devhosts <ws_url> <session_file>".to_string());
    };
    let mut connection = connect_from_session_file(ws_url, session_file).await?;
    let devhosts = connection.list_devhosts().await.map_err(|error| format!("list_devhosts failed: {error}"))?;
    println!("{}", serde_json::to_string_pretty(&devhosts).unwrap());
    Ok(())
}

async fn create_workspace(args: &[String]) -> Result<(), String> {
    let [ws_url, session_file, devhost_id, workspace_name, clone_url] = args else {
        return Err("usage: create-workspace <ws_url> <session_file> <devhost_id> <workspace_name> <clone_url>".to_string());
    };
    let mut connection = connect_from_session_file(ws_url, session_file).await?;
    let request = RpcRequest::WorkspaceCreate {
        request_id: "dev-cli-workspace-create".to_string(),
        devhost_id: devhost_id.clone(),
        workspace_name: workspace_name.clone(),
        project_source: ProjectSource::CloneUrl { clone_url: clone_url.clone() },
        parent_workspace_id: None,
    };
    let response = connection.call_rpc(devhost_id, request).await.map_err(|error| format!("call_rpc failed: {error}"))?;
    print_response(&response)
}

async fn create_item(args: &[String]) -> Result<(), String> {
    let [ws_url, session_file, workspace_id, devhost_id, name, agent] = args else {
        return Err("usage: create-item <ws_url> <session_file> <workspace_id> <devhost_id> <name> <agent>".to_string());
    };
    let agent_kind = match agent.as_str() {
        "claude" => AgentKind::Claude,
        "codex" => AgentKind::Codex,
        "opencode" => AgentKind::Opencode,
        other => return Err(format!("unknown agent kind: {other}")),
    };
    let mut connection = connect_from_session_file(ws_url, session_file).await?;
    let request = RpcRequest::ItemCreate {
        request_id: "dev-cli-item-create".to_string(),
        workspace_id: workspace_id.clone(),
        item_type: ItemType::AgentTerminal,
        name: name.clone(),
        agent: Some(agent_kind),
        command: None,
        port: None,
    };
    let response = connection.call_rpc(devhost_id, request).await.map_err(|error| format!("call_rpc failed: {error}"))?;
    print_response(&response)
}

fn print_response(response: &RpcResponse) -> Result<(), String> {
    println!("{}", serde_json::to_string_pretty(response).unwrap());
    match response {
        RpcResponse::Error { code, message, .. } => Err(format!("{code}: {message}")),
        _ => Ok(()),
    }
}
