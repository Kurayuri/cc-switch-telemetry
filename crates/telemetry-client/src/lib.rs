use anyhow::Context;
use rusqlite::{Connection, Row};
use serde::{Deserialize, Serialize};
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};
use telemetry_core::{event_id, EventBatch, UsageEvent, SCHEMA_VERSION};

#[derive(Debug, Clone)]
pub struct ClientConfig {
    pub cc_switch_db: PathBuf,
    pub server_url: String,
    pub node_id: String,
    pub auth_token: Option<String>,
    pub batch_size: usize,
    pub overlap_seconds: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct Cursor {
    pub created_at: i64,
    pub request_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileFingerprint {
    pub modified: SystemTime,
    pub len: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatabaseFingerprint {
    pub database: FileFingerprint,
    pub wal: Option<FileFingerprint>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncSummary {
    pub sent: usize,
    pub accepted: usize,
    pub duplicates: usize,
    pub rejected: usize,
    pub cursor_advanced: bool,
}

fn file_fingerprint(path: &Path) -> std::io::Result<FileFingerprint> {
    let metadata = std::fs::metadata(path)?;
    Ok(FileFingerprint {
        modified: metadata.modified()?,
        len: metadata.len(),
    })
}

fn wal_path(database: &Path) -> PathBuf {
    let mut path = OsString::from(database.as_os_str());
    path.push("-wal");
    PathBuf::from(path)
}

pub fn database_fingerprint(path: &Path) -> std::io::Result<DatabaseFingerprint> {
    let database = file_fingerprint(path)?;
    let wal = match file_fingerprint(&wal_path(path)) {
        Ok(fingerprint) => Some(fingerprint),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    Ok(DatabaseFingerprint { database, wal })
}

pub fn load_cursor(path: &std::path::Path) -> anyhow::Result<Cursor> {
    match std::fs::read_to_string(path) {
        Ok(contents) => Ok(serde_json::from_str(&contents)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Cursor::default()),
        Err(error) => Err(error.into()),
    }
}

pub fn save_cursor(path: &std::path::Path, cursor: &Cursor) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("tmp");
    std::fs::write(&temporary, serde_json::to_vec(cursor)?)?;
    std::fs::rename(temporary, path)?;
    Ok(())
}

fn event_from_row(row: &Row<'_>, node_id: &str) -> rusqlite::Result<UsageEvent> {
    let request_id: String = row.get("request_id")?;
    Ok(UsageEvent {
        event_id: event_id(node_id, &request_id),
        node_id: node_id.into(),
        request_id,
        created_at: row.get("created_at")?,
        app_type: row.get("app_type")?,
        provider_id: row.get("provider_id")?,
        model: row.get("model")?,
        request_model: row.get("request_model")?,
        pricing_model: row.get("pricing_model")?,
        input_tokens: row.get("input_tokens")?,
        output_tokens: row.get("output_tokens")?,
        cache_read_tokens: row.get("cache_read_tokens")?,
        cache_creation_tokens: row.get("cache_creation_tokens")?,
        input_token_semantics: row.get("input_token_semantics")?,
        total_cost_usd: row.get("total_cost_usd")?,
        latency_ms: row.get("latency_ms")?,
        status_code: row.get("status_code")?,
        is_streaming: row.get::<_, i64>("is_streaming")? != 0,
        data_source: row.get("data_source")?,
    })
}

pub fn read_events(config: &ClientConfig, cursor: &Cursor) -> anyhow::Result<Vec<UsageEvent>> {
    let conn = Connection::open_with_flags(
        &config.cc_switch_db,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
    .context("open cc-switch db read-only")?;
    conn.busy_timeout(Duration::from_secs(2))?;
    let has_input_semantics = conn
        .prepare("PRAGMA table_info(proxy_request_logs)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?
        .iter()
        .any(|column| column == "input_token_semantics");
    let semantics_column = if has_input_semantics {
        "input_token_semantics"
    } else {
        "0 AS input_token_semantics"
    };
    let sql = format!(
        "SELECT request_id, created_at, app_type, provider_id, model, request_model, \
         pricing_model, input_tokens, output_tokens, cache_read_tokens, \
         cache_creation_tokens, {semantics_column}, total_cost_usd, latency_ms, \
         status_code, is_streaming, data_source FROM proxy_request_logs \
         WHERE (created_at > ?1 OR (created_at = ?1 AND request_id > ?2)) \
         ORDER BY created_at, request_id LIMIT ?3"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(
        rusqlite::params![
            cursor.created_at,
            cursor.request_id,
            config.batch_size as i64
        ],
        |row| event_from_row(row, &config.node_id),
    )?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

pub async fn upload(
    config: &ClientConfig,
    events: Vec<UsageEvent>,
) -> anyhow::Result<telemetry_core::BatchResponse> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()?;
    let mut request = client
        .post(format!(
            "{}/v1/events/batch",
            config.server_url.trim_end_matches('/')
        ))
        .json(&EventBatch {
            schema_version: SCHEMA_VERSION,
            node_id: config.node_id.clone(),
            events,
        });
    if let Some(token) = &config.auth_token {
        request = request.bearer_auth(token);
    }
    let response = request.send().await?.error_for_status()?;
    Ok(response.json().await?)
}

fn overlap_start(cursor: &Cursor, overlap_seconds: i64) -> Cursor {
    Cursor {
        created_at: cursor.created_at.saturating_sub(overlap_seconds),
        request_id: String::new(),
    }
}

pub async fn sync_available(
    config: &ClientConfig,
    cursor: &mut Cursor,
) -> anyhow::Result<SyncSummary> {
    if config.batch_size == 0 {
        anyhow::bail!("batch_size must be greater than zero");
    }
    let original_cursor = cursor.clone();
    let mut scan_cursor = overlap_start(cursor, config.overlap_seconds);
    let mut summary = SyncSummary::default();
    loop {
        let events = read_events(config, &scan_cursor)?;
        if events.is_empty() {
            break;
        }
        let sent = events.len();
        let response = upload(config, events.clone()).await?;
        let acknowledged =
            response.accepted.len() + response.duplicates.len() + response.rejected.len();
        if acknowledged != sent {
            anyhow::bail!(
                "server acknowledgement mismatch: sent={sent} acknowledged={acknowledged}"
            );
        }
        summary.sent += sent;
        summary.accepted += response.accepted.len();
        summary.duplicates += response.duplicates.len();
        summary.rejected += response.rejected.len();
        if !response.rejected.is_empty() {
            anyhow::bail!(
                "server rejected {} of {sent} usage events",
                response.rejected.len()
            );
        }
        if let Some(last) = events.last() {
            scan_cursor = Cursor {
                created_at: last.created_at,
                request_id: last.request_id.clone(),
            };
            if scan_cursor > *cursor {
                *cursor = scan_cursor.clone();
            }
        }
        if sent < config.batch_size {
            break;
        }
    }
    summary.cursor_advanced = *cursor > original_cursor;
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        net::SocketAddr,
        sync::{Arc, Mutex},
    };
    #[test]
    fn cursor_reads_same_second_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db.sqlite");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("CREATE TABLE proxy_request_logs (request_id TEXT PRIMARY KEY, created_at INTEGER, app_type TEXT, provider_id TEXT, model TEXT, request_model TEXT, pricing_model TEXT, input_tokens INTEGER, output_tokens INTEGER, cache_read_tokens INTEGER, cache_creation_tokens INTEGER, total_cost_usd TEXT, latency_ms INTEGER, status_code INTEGER, is_streaming INTEGER, data_source TEXT); INSERT INTO proxy_request_logs VALUES ('a',10,'x','p','m','','',1,1,0,0,'0',1,200,0,'proxy'),('b',10,'x','p','m','','',1,1,0,0,'0',1,200,0,'proxy');").unwrap();
        let config = ClientConfig {
            cc_switch_db: path,
            server_url: "http://localhost".into(),
            node_id: "n".into(),
            auth_token: None,
            batch_size: 10,
            overlap_seconds: 0,
        };
        let events = read_events(&config, &Cursor::default()).unwrap();
        assert_eq!(events[0].input_token_semantics, 0);
        assert_eq!(
            events
                .iter()
                .map(|e| e.request_id.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b"]
        );
    }

    #[test]
    fn fingerprint_detects_wal_changes() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("cc-switch.db");
        std::fs::write(&database, b"db").unwrap();
        let before = database_fingerprint(&database).unwrap();
        assert!(before.wal.is_none());
        std::fs::write(wal_path(&database), b"wal-data").unwrap();
        let after_create = database_fingerprint(&database).unwrap();
        assert_ne!(before, after_create);
        std::fs::write(wal_path(&database), b"wal-data-extended").unwrap();
        let after_write = database_fingerprint(&database).unwrap();
        assert_ne!(after_create, after_write);
    }

    #[test]
    fn overlap_scan_start_never_moves_persistent_cursor() {
        let cursor = Cursor {
            created_at: 1_000,
            request_id: "request-z".into(),
        };
        let start = overlap_start(&cursor, 600);
        assert_eq!(start.created_at, 400);
        assert!(start.request_id.is_empty());
        assert_eq!(cursor.created_at, 1_000);
    }

    #[tokio::test]
    async fn sync_drains_backlog_and_recovers_late_events() {
        let directory = tempfile::tempdir().unwrap();
        let source_path = directory.path().join("cc-switch.db");
        let mut source = Connection::open(&source_path).unwrap();
        source
            .execute_batch(
                "CREATE TABLE proxy_request_logs (
                    request_id TEXT PRIMARY KEY,
                    created_at INTEGER,
                    app_type TEXT,
                    provider_id TEXT,
                    model TEXT,
                    request_model TEXT,
                    pricing_model TEXT,
                    input_tokens INTEGER,
                    output_tokens INTEGER,
                    cache_read_tokens INTEGER,
                    cache_creation_tokens INTEGER,
                    input_token_semantics INTEGER,
                    total_cost_usd TEXT,
                    latency_ms INTEGER,
                    status_code INTEGER,
                    is_streaming INTEGER,
                    data_source TEXT
                );",
            )
            .unwrap();
        let transaction = source.transaction().unwrap();
        for index in 1..=1_201 {
            transaction
                .execute(
                    "INSERT INTO proxy_request_logs VALUES (
                        ?1,?2,'codex','provider','model','','',1,1,0,0,1,
                        '0',0,200,1,'proxy'
                    )",
                    rusqlite::params![format!("request-{index:04}"), index],
                )
                .unwrap();
        }
        transaction.commit().unwrap();

        let server_path = directory.path().join("server.db");
        let server_connection = telemetry_server::init_db(&server_path).unwrap();
        let server_state = telemetry_server::ServerState {
            db: Arc::new(Mutex::new(server_connection)),
            db_path: server_path.clone(),
            auth_token: None,
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                telemetry_server::router(server_state)
                    .into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .unwrap();
        });

        let config = ClientConfig {
            cc_switch_db: source_path,
            server_url: format!("http://{address}"),
            node_id: "node-a".into(),
            auth_token: None,
            batch_size: 512,
            overlap_seconds: 600,
        };
        let mut cursor = Cursor::default();
        let initial = sync_available(&config, &mut cursor).await.unwrap();
        assert_eq!(initial.sent, 1_201);
        assert_eq!(initial.accepted, 1_201);
        assert_eq!(initial.duplicates, 0);
        assert!(initial.cursor_advanced);
        assert_eq!(cursor.created_at, 1_201);

        source
            .execute(
                "INSERT INTO proxy_request_logs VALUES (
                    'late-event',1000,'codex','provider','model','','',1,1,0,0,1,
                    '0',0,200,1,'session_log'
                )",
                [],
            )
            .unwrap();
        let cursor_before_late_scan = cursor.clone();
        let late = sync_available(&config, &mut cursor).await.unwrap();
        assert_eq!(late.sent, 602);
        assert_eq!(late.accepted, 1);
        assert_eq!(late.duplicates, 601);
        assert!(!late.cursor_advanced);
        assert_eq!(cursor, cursor_before_late_scan);

        let collected: i64 = Connection::open(&server_path)
            .unwrap()
            .query_row("SELECT COUNT(*) FROM usage_events", [], |row| row.get(0))
            .unwrap();
        assert_eq!(collected, 1_202);
        server.abort();
    }
}
