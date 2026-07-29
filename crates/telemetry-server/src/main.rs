use std::{net::SocketAddr, path::PathBuf};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let db = std::env::var_os("TELEMETRY_DB")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("./data/telemetry.db"));
    let listen: SocketAddr = std::env::var("TELEMETRY_LISTEN")
        .unwrap_or_else(|_| "127.0.0.1:8787".into())
        .parse()?;
    telemetry_server::serve(db, listen, std::env::var("TELEMETRY_TOKEN").ok()).await
}
