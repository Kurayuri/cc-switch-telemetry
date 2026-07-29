use std::{path::PathBuf, time::Duration};
use telemetry_client::{
    database_fingerprint, load_cursor, save_cursor, sync_available, ClientConfig,
    DatabaseFingerprint,
};

const POLL_INTERVAL: Duration = Duration::from_secs(5);

async fn sync_until_stable(
    config: &ClientConfig,
    state_path: &std::path::Path,
    cursor: &mut telemetry_client::Cursor,
) -> anyhow::Result<DatabaseFingerprint> {
    loop {
        let before = database_fingerprint(&config.cc_switch_db)?;
        let summary = sync_available(config, cursor).await?;
        if summary.cursor_advanced {
            save_cursor(state_path, cursor)?;
        }
        if summary.sent > 0 {
            eprintln!(
                "usage sync: sent={} accepted={} duplicates={} rejected={}",
                summary.sent, summary.accepted, summary.duplicates, summary.rejected
            );
        }
        let after = database_fingerprint(&config.cc_switch_db)?;
        if before == after {
            return Ok(after);
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = ClientConfig {
        cc_switch_db: std::env::var_os("CC_SWITCH_DB")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("~/.cc-switch/cc-switch.db")),
        server_url: std::env::var("TELEMETRY_SERVER_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:8787".into()),
        node_id: std::env::var("TELEMETRY_NODE_ID").unwrap_or_else(|_| "node-1".into()),
        auth_token: std::env::var("TELEMETRY_TOKEN").ok(),
        batch_size: 512,
        overlap_seconds: 600,
    };
    let state_path = std::env::var_os("TELEMETRY_STATE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("./data/client-cursor.json"));
    let mut cursor = load_cursor(&state_path)?;
    let mut observed: Option<DatabaseFingerprint> = None;
    loop {
        let changed = match database_fingerprint(&config.cc_switch_db) {
            Ok(current) => observed.as_ref() != Some(&current),
            Err(error) => {
                eprintln!("client metadata error: {error}");
                true
            }
        };
        if changed {
            match sync_until_stable(&config, &state_path, &mut cursor).await {
                Ok(current) => observed = Some(current),
                Err(error) => {
                    observed = None;
                    eprintln!("client sync error: {error}");
                }
            }
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}
