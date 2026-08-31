use std::{net::SocketAddr, path::PathBuf};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if let [command, from_flag, from, to_flag, to] = args.as_slice() {
        if command == "rebuild-v2" && from_flag == "--from" && to_flag == "--to" {
            let summary = telemetry_server::rebuild_v2_metadata(from, to)?;
            eprintln!(
                "rebuilt protocol-v2 metadata database: target={} nodes={} providers={}",
                to, summary.nodes, summary.providers
            );
            return Ok(());
        }
    }
    if !args.is_empty() {
        anyhow::bail!("usage: telemetry-server [rebuild-v2 --from OLD_DB --to NEW_DB]");
    }
    let db = std::env::var_os("TELEMETRY_DB")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("./data/telemetry.db"));
    let listen_text = std::env::var("TELEMETRY_LISTEN").unwrap_or_else(|_| "127.0.0.1:8787".into());
    let listen: SocketAddr = listen_text
        .parse()
        .map_err(|error| anyhow::anyhow!("invalid TELEMETRY_LISTEN={listen_text:?}: {error}"))?;
    let admin_password = std::env::var("ADMIN_PASSWORD")
        .ok()
        .filter(|password| !password.is_empty());
    eprintln!(
        "telemetry-server starting: db={} listen={} node_auth=database admin={}",
        db.display(),
        listen,
        if admin_password.is_some() {
            "enabled"
        } else {
            "disabled"
        }
    );
    telemetry_server::serve(db, listen, admin_password).await
}
