use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeRecord {
    pub uuid: String,
    pub node_name: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub has_token: bool,
}

pub fn ensure_schema(conn: &Connection) -> anyhow::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS nodes (
             uuid TEXT PRIMARY KEY,
             node_name TEXT NOT NULL,
             token_hash TEXT NOT NULL DEFAULT '',
             created_at INTEGER NOT NULL,
             updated_at INTEGER NOT NULL
         );
         CREATE UNIQUE INDEX IF NOT EXISTS idx_nodes_node_name
             ON nodes(node_name);
         CREATE INDEX IF NOT EXISTS idx_nodes_updated
             ON nodes(updated_at);",
    )?;

    let mut legacy_values = BTreeSet::new();
    for table in [
        "usage_events",
        "provider_catalog",
        "usage_daily_snapshots",
        "ingest_batches",
    ] {
        let exists: bool = conn.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM sqlite_master
                 WHERE type = 'table' AND name = ?1
             )",
            params![table],
            |row| row.get(0),
        )?;
        if !exists {
            continue;
        }
        let sql = format!(
            "SELECT DISTINCT node_id FROM {table}
             WHERE TRIM(node_id) <> ''"
        );
        let mut statement = conn.prepare(&sql)?;
        let values = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        legacy_values.extend(values);
    }
    if legacy_values.is_empty() {
        return Ok(());
    }

    let tx = conn.unchecked_transaction()?;
    for legacy in legacy_values {
        let existing: Option<String> = tx
            .query_row(
                "SELECT uuid FROM nodes
                 WHERE uuid = ?1 OR node_name = ?1
                 ORDER BY CASE WHEN uuid = ?1 THEN 0 ELSE 1 END
                 LIMIT 1",
                params![legacy],
                |row| row.get(0),
            )
            .optional()?;
        let uuid = match existing {
            Some(uuid) => uuid,
            None => {
                let uuid = Uuid::new_v4().to_string();
                let now = Utc::now().timestamp();
                tx.execute(
                    "INSERT INTO nodes (uuid, node_name, token_hash, created_at, updated_at)
                     VALUES (?1, ?2, '', ?3, ?3)",
                    params![uuid, legacy, now],
                )?;
                uuid
            }
        };
        if uuid == legacy {
            continue;
        }

        // Keep event ids and rollup keys idempotent with the new node UUID.
        tx.execute(
            "UPDATE usage_events
             SET event_id = ?1 || ':' || request_id, node_id = ?1
             WHERE node_id = ?2",
            params![uuid, legacy],
        )?;
        tx.execute(
            "UPDATE provider_catalog SET node_id = ?1 WHERE node_id = ?2",
            params![uuid, legacy],
        )?;
        tx.execute(
            "UPDATE usage_daily_snapshots
             SET snapshot_key = ?1 || substr(snapshot_key, length(?2) + 1), node_id = ?1
             WHERE node_id = ?2",
            params![uuid, legacy],
        )?;
        tx.execute(
            "UPDATE ingest_batches SET node_id = ?1 WHERE node_id = ?2",
            params![uuid, legacy],
        )?;
    }
    tx.commit()?;
    Ok(())
}

pub fn generate_token() -> String {
    format!("tl_{}", Uuid::new_v4().simple())
}

pub fn token_hash(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn constant_time_equal(left: &str, right: &str) -> bool {
    let mut difference = left.len() ^ right.len();
    for (left, right) in left.bytes().zip(right.bytes()) {
        difference |= usize::from(left ^ right);
    }
    difference == 0
}

fn node_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<NodeRecord> {
    Ok(NodeRecord {
        uuid: row.get(0)?,
        node_name: row.get(1)?,
        created_at: row.get(2)?,
        updated_at: row.get(3)?,
        has_token: row.get::<_, i64>(4)? != 0,
    })
}

pub fn list(conn: &Connection) -> rusqlite::Result<Vec<NodeRecord>> {
    let mut statement = conn.prepare(
        "SELECT uuid, node_name, created_at, updated_at,
                CASE WHEN token_hash <> '' THEN 1 ELSE 0 END
         FROM nodes ORDER BY node_name COLLATE NOCASE, uuid",
    )?;
    let result = statement
        .query_map([], node_from_row)?
        .collect::<Result<Vec<_>, _>>();
    result
}

pub fn create(conn: &Connection, node_name: &str) -> rusqlite::Result<(NodeRecord, String)> {
    let uuid = Uuid::new_v4().to_string();
    let token = generate_token();
    let now = Utc::now().timestamp();
    conn.execute(
        "INSERT INTO nodes (uuid, node_name, token_hash, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?4)",
        params![uuid, node_name, token_hash(&token), now],
    )?;
    Ok((
        NodeRecord {
            uuid,
            node_name: node_name.to_owned(),
            created_at: now,
            updated_at: now,
            has_token: true,
        },
        token,
    ))
}

pub fn rename(
    conn: &Connection,
    uuid: &str,
    node_name: &str,
) -> rusqlite::Result<Option<NodeRecord>> {
    let now = Utc::now().timestamp();
    let changed = conn.execute(
        "UPDATE nodes SET node_name = ?1, updated_at = ?2 WHERE uuid = ?3",
        params![node_name, now, uuid],
    )?;
    if changed == 0 {
        return Ok(None);
    }
    get(conn, uuid)
}

pub fn regenerate_token(
    conn: &Connection,
    uuid: &str,
) -> rusqlite::Result<Option<(NodeRecord, String)>> {
    let token = generate_token();
    let now = Utc::now().timestamp();
    let changed = conn.execute(
        "UPDATE nodes SET token_hash = ?1, updated_at = ?2 WHERE uuid = ?3",
        params![token_hash(&token), now, uuid],
    )?;
    if changed == 0 {
        return Ok(None);
    }
    Ok(get(conn, uuid)?.map(|node| (node, token)))
}

pub fn revoke_token(conn: &Connection, uuid: &str) -> rusqlite::Result<Option<NodeRecord>> {
    let now = Utc::now().timestamp();
    let changed = conn.execute(
        "UPDATE nodes SET token_hash = '', updated_at = ?1 WHERE uuid = ?2",
        params![now, uuid],
    )?;
    if changed == 0 {
        return Ok(None);
    }
    get(conn, uuid)
}

pub fn get(conn: &Connection, uuid: &str) -> rusqlite::Result<Option<NodeRecord>> {
    conn.query_row(
        "SELECT uuid, node_name, created_at, updated_at,
                CASE WHEN token_hash <> '' THEN 1 ELSE 0 END
         FROM nodes WHERE uuid = ?1",
        params![uuid],
        node_from_row,
    )
    .optional()
}

pub fn authorized_node(conn: &Connection, uuid: &str, token: &str) -> bool {
    let expected: Option<String> = conn
        .query_row(
            "SELECT token_hash FROM nodes WHERE uuid = ?1 AND token_hash <> ''",
            params![uuid],
            |row| row.get(0),
        )
        .optional()
        .ok()
        .flatten();
    expected.is_some_and(|expected| constant_time_equal(&expected, &token_hash(token)))
}

pub fn authorized_uuid(conn: &Connection, token: &str) -> Option<String> {
    let expected = token_hash(token);
    conn.query_row(
        "SELECT uuid FROM nodes WHERE token_hash = ?1 AND token_hash <> ''",
        params![expected],
        |row| row.get(0),
    )
    .optional()
    .ok()
    .flatten()
}

pub fn authorized_any(conn: &Connection, token: &str) -> bool {
    authorized_uuid(conn, token).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_node_ids_migrate_to_stable_uuid_nodes() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE usage_events (
                 event_id TEXT PRIMARY KEY, node_id TEXT NOT NULL,
                 request_id TEXT NOT NULL, created_at INTEGER NOT NULL,
                 UNIQUE(node_id, request_id)
             );
             CREATE TABLE provider_catalog (
                 node_id TEXT NOT NULL, app_type TEXT NOT NULL,
                 provider_id TEXT NOT NULL, name TEXT NOT NULL,
                 updated_at INTEGER NOT NULL,
                 PRIMARY KEY(node_id, app_type, provider_id)
             );
             CREATE TABLE usage_daily_snapshots (
                 snapshot_key TEXT PRIMARY KEY, node_id TEXT NOT NULL
             );
             CREATE TABLE ingest_batches (
                 batch_id TEXT PRIMARY KEY, node_id TEXT NOT NULL,
                 received_at INTEGER NOT NULL, event_count INTEGER NOT NULL
             );
             INSERT INTO usage_events VALUES ('old:r1','old', 'r1', 1);
             INSERT INTO provider_catalog VALUES ('old','a','p','P',1);
             INSERT INTO usage_daily_snapshots VALUES ('old|2026-01-01','old');
             INSERT INTO ingest_batches VALUES ('b','old',1,1);",
        )
        .unwrap();

        ensure_schema(&conn).unwrap();
        let node: (String, String) = conn
            .query_row("SELECT uuid,node_name FROM nodes", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .unwrap();
        assert_ne!(node.0, "old");
        assert_eq!(node.1, "old");
        let migrated: String = conn
            .query_row("SELECT node_id FROM usage_events", [], |row| row.get(0))
            .unwrap();
        assert_eq!(migrated, node.0);
        let event_id: String = conn
            .query_row("SELECT event_id FROM usage_events", [], |row| row.get(0))
            .unwrap();
        assert_eq!(event_id, format!("{}:r1", node.0));
        let snapshot_key: String = conn
            .query_row(
                "SELECT snapshot_key FROM usage_daily_snapshots",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(snapshot_key, format!("{}|2026-01-01", node.0));

        ensure_schema(&conn).unwrap();
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM nodes", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            1
        );
    }

    #[test]
    fn token_lifecycle_authorizes_only_current_token() {
        let conn = Connection::open_in_memory().unwrap();
        ensure_schema(&conn).unwrap();
        let (node, token) = create(&conn, "node").unwrap();
        assert!(authorized_node(&conn, &node.uuid, &token));
        assert!(!authorized_node(&conn, &node.uuid, "wrong"));
        let (_, replacement) = regenerate_token(&conn, &node.uuid).unwrap().unwrap();
        assert!(!authorized_node(&conn, &node.uuid, &token));
        assert!(authorized_node(&conn, &node.uuid, &replacement));
        revoke_token(&conn, &node.uuid).unwrap();
        assert!(!authorized_node(&conn, &node.uuid, &replacement));
    }
}
