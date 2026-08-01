use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use telemetry_client::usage_ledger::{self, LocalUsageConfig};
use telemetry_client::{
    database_fingerprint, load_cursor, save_cursor, sync_available, sync_provider_catalog,
    ClientConfig, Cursor, DatabaseFingerprint,
};

const POLL_INTERVAL: Duration = Duration::from_secs(5);

fn client_config(database: PathBuf) -> ClientConfig {
    ClientConfig {
        cc_switch_db: database,
        server_url: std::env::var("TELEMETRY_SERVER_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:8787".into()),
        node_id: std::env::var("TELEMETRY_NODE_ID").unwrap_or_else(|_| "node-1".into()),
        auth_token: std::env::var("TELEMETRY_TOKEN").ok(),
        batch_size: 512,
        overlap_seconds: 0,
    }
}

fn upload_state_path() -> PathBuf {
    std::env::var_os("TELEMETRY_UPLOAD_STATE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("./data/client-upload-cursor.json"))
}

async fn upload_ledger(
    upload_config: &ClientConfig,
    provider_config: &ClientConfig,
    state_path: &Path,
    cursor: &mut Cursor,
) -> anyhow::Result<()> {
    let provider_count = sync_provider_catalog(provider_config).await?;
    if provider_count > 0 {
        eprintln!("provider sync: {provider_count} mapped providers");
    }
    let summary = sync_available(upload_config, cursor).await?;
    if summary.cursor_advanced {
        save_cursor(state_path, cursor)?;
    }
    if summary.sent > 0 {
        eprintln!(
            "usage sync: sent={} accepted={} duplicates={} rejected={}",
            summary.sent, summary.accepted, summary.duplicates, summary.rejected
        );
    }
    Ok(())
}

fn source_config(source: &str, local: &LocalUsageConfig) -> anyhow::Result<ClientConfig> {
    let database = match source {
        "local" => local.database.clone(),
        "cc-switch" => std::env::var_os("CC_SWITCH_DB")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("~/.cc-switch/cc-switch.db")),
        _ => anyhow::bail!("unknown source {source}; expected local or cc-switch"),
    };
    Ok(client_config(database))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let local = LocalUsageConfig::from_env();
    let rebuild = args.first().is_some_and(|arg| arg == "rebuild");
    let upload_after_rebuild = rebuild && args.iter().skip(1).any(|arg| arg == "--upload");
    if rebuild {
        if args.iter().skip(1).any(|arg| arg != "--upload") {
            anyhow::bail!("usage: telemetry-client rebuild [--upload]");
        }
        let summary = usage_ledger::rebuild(&local).await?;
        let state_path = upload_state_path();
        if state_path.exists() {
            std::fs::remove_file(&state_path)?;
        }
        eprintln!(
            "client ledger rebuilt: path={} imported={} skipped={}",
            local.database.display(),
            summary.imported,
            summary.skipped
        );
        if upload_after_rebuild {
            let upload_config = client_config(local.database.clone());
            let mut cursor = Cursor::default();
            eprintln!(
                "client upload: ledger={} state={} cursor=reset",
                local.database.display(),
                state_path.display()
            );
            upload_ledger(&upload_config, &upload_config, &state_path, &mut cursor).await?;
        }
        return Ok(());
    }

    let source = args
        .windows(2)
        .find(|pair| pair[0] == "--source")
        .map(|pair| pair[1].as_str())
        .unwrap_or("cc-switch");
    if args
        .first()
        .is_some_and(|arg| arg != "run" && arg != "--source")
        || args.len() > 3
        || (args.first().is_some_and(|arg| arg == "run") && args.len() > 1 && args[1] != "--source")
    {
        anyhow::bail!(
            "usage: telemetry-client [rebuild [--upload] | run --source local|cc-switch]"
        );
    }

    let source_config = source_config(source, &local)?;
    let upload_config = client_config(local.database.clone());
    let state_path = upload_state_path();
    let mut upload_cursor = load_cursor(&state_path)?;
    eprintln!(
        "telemetry-client starting: source={} source_db={} ledger={} upload_state={} upload_cursor=({}, {})",
        source,
        source_config.cc_switch_db.display(),
        local.database.display(),
        state_path.display(),
        upload_cursor.created_at,
        upload_cursor.request_id,
    );

    let mut observed: Option<DatabaseFingerprint> = None;
    loop {
        let changed = match database_fingerprint(&source_config.cc_switch_db) {
            Ok(current) => observed.as_ref() != Some(&current),
            Err(error) => {
                eprintln!("client source metadata error: {error}");
                true
            }
        };
        if changed {
            let update = if source == "local" {
                usage_ledger::sync_local(&local).await
            } else {
                usage_ledger::sync_cc_switch(&source_config, &local.database)
            };
            match update {
                Ok(summary) => {
                    if summary.imported > 0 || summary.skipped > 0 {
                        eprintln!(
                            "client ledger update: source={} imported={} skipped={}",
                            source, summary.imported, summary.skipped
                        );
                    }
                    match upload_ledger(
                        &upload_config,
                        &source_config,
                        &state_path,
                        &mut upload_cursor,
                    )
                    .await
                    {
                        Ok(()) => observed = database_fingerprint(&source_config.cc_switch_db).ok(),
                        Err(error) => eprintln!("client upload error: {error}"),
                    }
                }
                Err(error) => eprintln!("client ledger update error: {error}"),
            }
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}
