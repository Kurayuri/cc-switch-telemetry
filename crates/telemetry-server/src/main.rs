use std::{net::SocketAddr, path::PathBuf};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let db = std::env::var_os("TELEMETRY_DB")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("./data/telemetry.db"));
    let listen_text = std::env::var("TELEMETRY_LISTEN").unwrap_or_else(|_| "127.0.0.1:8787".into());
    let listen: SocketAddr = listen_text
        .parse()
        .map_err(|error| anyhow::anyhow!("invalid TELEMETRY_LISTEN={listen_text:?}: {error}"))?;
    let has_token = std::env::var_os("TELEMETRY_TOKEN").is_some();
    eprintln!(
        "telemetry-server starting: db={} listen={} auth={}",
        db.display(),
        listen,
        if has_token { "enabled" } else { "disabled" }
    );
    telemetry_server::serve(db, listen, std::env::var("TELEMETRY_TOKEN").ok()).await
}
