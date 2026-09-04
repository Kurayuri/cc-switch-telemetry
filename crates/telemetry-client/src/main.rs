use std::{path::PathBuf, time::Duration};
use telemetry_client::quota::{self, QuotaConfig};
use telemetry_client::usage_ledger::{self, LocalUsageConfig};
use telemetry_client::{
    database_fingerprint, sync_snapshot_v3_with_mode, verify_cc_switch_mirror, ClientConfig,
    DatabaseFingerprint,
};

const POLL_INTERVAL: Duration = Duration::from_secs(5);

fn client_config(database: PathBuf) -> anyhow::Result<ClientConfig> {
    let auth_token = std::env::var("TELEMETRY_TOKEN")
        .ok()
        .filter(|token| !token.is_empty())
        .ok_or_else(|| anyhow::anyhow!("TELEMETRY_TOKEN must be set for client uploads"))?;
    Ok(ClientConfig {
        cc_switch_db: database,
        server_url: std::env::var("TELEMETRY_SERVER_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:8787".into()),
        auth_token,
        batch_size: 512,
    })
}

fn source_database(source: &str, local: &LocalUsageConfig) -> anyhow::Result<PathBuf> {
    let database = match source {
        "local" | "local-compact" => local.database.clone(),
        "cc-switch" => std::env::var_os("CC_SWITCH_DB")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                dirs::home_dir()
                    .unwrap_or_else(|| PathBuf::from("."))
                    .join(".cc-switch/cc-switch.db")
            }),
        _ => anyhow::bail!("unknown source {source}; expected local, local-compact, or cc-switch"),
    };
    Ok(database)
}

fn source_config(source: &str, local: &LocalUsageConfig) -> anyhow::Result<ClientConfig> {
    client_config(source_database(source, local)?)
}

fn selected_source(args: &[String]) -> &str {
    args.windows(2)
        .find(|pair| pair[0] == "--source")
        .map(|pair| pair[1].as_str())
        .unwrap_or("cc-switch")
}

async fn upload_ledger(
    upload_config: &ClientConfig,
    provider_config: &ClientConfig,
    source: &str,
    force_replace_all: bool,
) -> anyhow::Result<()> {
    let result =
        sync_snapshot_v3_with_mode(upload_config, provider_config, source, force_replace_all)
            .await?;
    eprintln!(
        "usage sync v3: inserted={} updated={} unchanged={} deleted={} rollups={} providers={}",
        result.inserted,
        result.updated,
        result.unchanged,
        result.deleted,
        result.rollups,
        result.providers
    );
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let local = LocalUsageConfig::from_env();
    let source = selected_source(&args);
    if !matches!(source, "cc-switch" | "local" | "local-compact") {
        anyhow::bail!("unknown source {source}; expected local, local-compact, or cc-switch");
    }
    if args.first().is_some_and(|arg| arg == "verify") {
        if args.as_slice() != ["verify", "--source", "cc-switch"] {
            anyhow::bail!("usage: telemetry-client verify --source cc-switch");
        }
        let read_config = ClientConfig {
            cc_switch_db: source_database("cc-switch", &local)?,
            server_url: String::new(),
            auth_token: String::new(),
            batch_size: 512,
        };
        let report = verify_cc_switch_mirror(&read_config, &local.database)?;
        eprintln!(
            "cc-switch mirror verified: source_db={} ledger={} detail_rows={} rollup_rows={}",
            read_config.cc_switch_db.display(),
            local.database.display(),
            report.detail_rows,
            report.rollup_rows
        );
        return Ok(());
    }
    if args.as_slice() == ["quota", "replay"] {
        let upload_config = client_config(local.database.clone())?;
        let quota_config = QuotaConfig::from_env(source_database("cc-switch", &local)?)?;
        quota::init_db(&quota_config.quota_db)?;
        let (accepted, duplicates) =
            quota::upload_pending(&quota_config, &upload_config, true).await?;
        eprintln!(
            "quota replay complete: database={} accepted={} duplicates={}",
            quota_config.quota_db.display(),
            accepted,
            duplicates
        );
        return Ok(());
    }
    let source_config = source_config(source, &local)?;
    let upload_config = client_config(local.database.clone())?;

    if args.first().is_some_and(|arg| arg == "rebuild") {
        let allowed = ["rebuild", "--source", source, "--replace-all", "--upload"];
        if args.iter().any(|arg| !allowed.contains(&arg.as_str()))
            || !args.iter().any(|arg| arg == "--replace-all")
        {
            anyhow::bail!(
                "usage: telemetry-client rebuild --source local|local-compact|cc-switch --replace-all [--upload]"
            );
        }
        let summary = match source {
            "local" => usage_ledger::rebuild(&local).await?,
            "local-compact" => usage_ledger::rebuild_compact(&local).await?,
            "cc-switch" => usage_ledger::rebuild_cc_switch(&source_config, &local.database)?,
            _ => unreachable!("source was validated above"),
        };
        eprintln!(
            "client ledger rebuilt: source={} path={} imported={} skipped={}",
            source,
            local.database.display(),
            summary.imported,
            summary.skipped
        );
        if args.iter().any(|arg| arg == "--upload") {
            let provider_config = if source == "cc-switch" {
                &source_config
            } else {
                &upload_config
            };
            upload_ledger(&upload_config, provider_config, source, true).await?;
        }
        return Ok(());
    }

    if args
        .first()
        .is_some_and(|arg| arg != "run" && arg != "--source")
        || args.len() > 3
        || (args.first().is_some_and(|arg| arg == "run") && args.len() > 1 && args[1] != "--source")
    {
        anyhow::bail!(
            "usage: telemetry-client [verify --source cc-switch | rebuild --source local|local-compact|cc-switch --replace-all [--upload] | quota replay | run --source local|local-compact|cc-switch]"
        );
    }

    eprintln!(
        "telemetry-client starting: protocol=v3 collector={} source_db={} ledger={}",
        source,
        source_config.cc_switch_db.display(),
        local.database.display(),
    );
    let quota_config = QuotaConfig::from_env(source_database("cc-switch", &local)?)?;
    let _quota_task = if quota_config.enabled() {
        match quota::init_db(&quota_config.quota_db) {
            Ok(_) => {
                eprintln!(
                    "quota collector enabled: interval={}s database={}",
                    quota_config.interval.as_secs(),
                    quota_config.quota_db.display()
                );
                let quota_upload = upload_config.clone();
                Some(tokio::spawn(quota::run(quota_config, quota_upload)))
            }
            Err(error) => {
                eprintln!("quota collector disabled: initialize database failed: {error}");
                None
            }
        }
    } else {
        eprintln!("quota collector disabled by TELEMETRY_QUOTA_INTERVAL_SECONDS=0");
        None
    };
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
            let update = match source {
                "local" => usage_ledger::sync_local(&local).await,
                "local-compact" => usage_ledger::sync_local_compact(&local).await,
                "cc-switch" => usage_ledger::sync_cc_switch(&source_config, &local.database),
                _ => unreachable!("source was validated above"),
            };
            match update {
                Ok(summary) => {
                    if summary.imported > 0 || summary.skipped > 0 {
                        eprintln!(
                            "client ledger update: source={} imported={} skipped={}",
                            source, summary.imported, summary.skipped
                        );
                    }
                    let provider_config = if source == "cc-switch" {
                        &source_config
                    } else {
                        &upload_config
                    };
                    match upload_ledger(&upload_config, provider_config, source, false).await {
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
