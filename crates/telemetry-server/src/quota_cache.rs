//! Rebuildable, transactionally maintained read projections. Raw observations remain authoritative.
use rusqlite::{Connection, OptionalExtension};

pub(crate) const ID: &str = "node_id,app_type,provider_id,metric_key,metric_kind,unit_key";
pub(crate) const COLUMNS: &str = "node_id,app_type,provider_id,metric_key,metric_kind,unit_key,sampled_at,observation_id,metric_label,unit,utilization_percent,used,remaining,total,resets_at";

pub(crate) fn ensure_schema(db: &Connection) -> anyhow::Result<()> {
    let fields = "node_id TEXT NOT NULL,app_type TEXT NOT NULL,provider_id TEXT NOT NULL,
        metric_key TEXT NOT NULL,metric_kind TEXT NOT NULL,unit_key TEXT NOT NULL,
        sampled_at INTEGER NOT NULL,observation_id TEXT NOT NULL,metric_label TEXT NOT NULL,
        unit TEXT,utilization_percent REAL,used REAL,remaining REAL,total REAL,resets_at INTEGER";
    let same = "node_id=OLD.node_id AND app_type=OLD.app_type AND provider_id=OLD.provider_id
        AND metric_key=OLD.metric_key AND metric_kind=OLD.metric_kind AND unit_key=OLD.unit_key";
    let updates = COLUMNS
        .split(',')
        .filter(|c| !ID.split(',').any(|k| k == *c))
        .map(|c| format!("{c}=excluded.{c}"))
        .collect::<Vec<_>>()
        .join(",");
    db.execute_batch(&format!("
        CREATE TABLE IF NOT EXISTS quota_sample_cache ({fields},
          PRIMARY KEY({ID},sampled_at,observation_id)) WITHOUT ROWID;
        CREATE INDEX IF NOT EXISTS idx_quota_sample_observation ON quota_sample_cache(node_id,observation_id);
        CREATE TABLE IF NOT EXISTS quota_current_cache ({fields},PRIMARY KEY({ID})) WITHOUT ROWID;
        CREATE TABLE IF NOT EXISTS quota_reset_cache (
          node_id TEXT NOT NULL,app_type TEXT NOT NULL,provider_id TEXT NOT NULL,
          metric_key TEXT NOT NULL,metric_kind TEXT NOT NULL,unit_key TEXT NOT NULL,
          revision INTEGER NOT NULL DEFAULT 0,cached_revision INTEGER NOT NULL DEFAULT -1,
          dirty_from INTEGER NOT NULL,through_at INTEGER NOT NULL DEFAULT -1,
          through_id TEXT NOT NULL DEFAULT '',runs TEXT NOT NULL DEFAULT '[]',
          PRIMARY KEY({ID})) WITHOUT ROWID;
        CREATE TABLE IF NOT EXISTS quota_projection_meta (id INTEGER PRIMARY KEY,backfill_row INTEGER NOT NULL,ready INTEGER NOT NULL);
        INSERT OR IGNORE INTO quota_projection_meta VALUES(1,0,0);
        CREATE TRIGGER IF NOT EXISTS quota_sample_insert AFTER INSERT ON quota_sample_cache BEGIN
          INSERT INTO quota_current_cache SELECT {COLUMNS} FROM quota_sample_cache
            WHERE node_id=NEW.node_id AND app_type=NEW.app_type AND provider_id=NEW.provider_id
              AND metric_key=NEW.metric_key AND metric_kind=NEW.metric_kind AND unit_key=NEW.unit_key
              AND sampled_at=NEW.sampled_at AND observation_id=NEW.observation_id
            ON CONFLICT({ID}) DO UPDATE SET {updates}
            WHERE (excluded.sampled_at,excluded.observation_id)>(quota_current_cache.sampled_at,quota_current_cache.observation_id);
          INSERT INTO quota_reset_cache({ID},dirty_from) VALUES(NEW.node_id,NEW.app_type,NEW.provider_id,NEW.metric_key,NEW.metric_kind,NEW.unit_key,NEW.sampled_at)
            ON CONFLICT({ID}) DO UPDATE SET revision=revision+1,dirty_from=MIN(dirty_from,excluded.dirty_from);
        END;
        CREATE TRIGGER IF NOT EXISTS quota_sample_delete AFTER DELETE ON quota_sample_cache BEGIN
          DELETE FROM quota_current_cache WHERE {same};
          INSERT INTO quota_current_cache SELECT {COLUMNS} FROM quota_sample_cache WHERE {same}
            ORDER BY sampled_at DESC,observation_id DESC LIMIT 1;
          UPDATE quota_reset_cache SET revision=revision+1,dirty_from=MIN(dirty_from,OLD.sampled_at) WHERE {same};
          DELETE FROM quota_reset_cache WHERE {same} AND NOT EXISTS(SELECT 1 FROM quota_current_cache WHERE {same});
        END;
        CREATE TRIGGER IF NOT EXISTS quota_metric_project AFTER INSERT ON quota_metrics BEGIN
          INSERT OR IGNORE INTO quota_sample_cache SELECT o.node_id,o.app_type,o.provider_id,
            NEW.metric_key,NEW.metric_kind,COALESCE(NEW.unit,''),o.sampled_at,o.observation_id,
            NEW.metric_label,NEW.unit,NEW.utilization_percent,NEW.used,NEW.remaining,NEW.total,NEW.resets_at
            FROM quota_observations o WHERE o.node_id=NEW.node_id AND o.observation_id=NEW.observation_id;
        END;
        CREATE TRIGGER IF NOT EXISTS quota_metric_unproject AFTER DELETE ON quota_metrics BEGIN
          DELETE FROM quota_sample_cache WHERE node_id=OLD.node_id AND observation_id=OLD.observation_id AND metric_key=OLD.metric_key;
        END;
        CREATE TRIGGER IF NOT EXISTS quota_metric_reproject AFTER UPDATE ON quota_metrics BEGIN
          DELETE FROM quota_sample_cache WHERE node_id=OLD.node_id AND observation_id=OLD.observation_id AND metric_key=OLD.metric_key;
          INSERT INTO quota_sample_cache SELECT o.node_id,o.app_type,o.provider_id,
            NEW.metric_key,NEW.metric_kind,COALESCE(NEW.unit,''),o.sampled_at,o.observation_id,
            NEW.metric_label,NEW.unit,NEW.utilization_percent,NEW.used,NEW.remaining,NEW.total,NEW.resets_at
            FROM quota_observations o WHERE o.node_id=NEW.node_id AND o.observation_id=NEW.observation_id;
        END;
    "))?;
    // Bounded, restartable migration. The listener is started after initialization.
    while db.query_row(
        "SELECT ready=0 FROM quota_projection_meta WHERE id=1",
        [],
        |r| r.get::<_, bool>(0),
    )? {
        let tx = db.unchecked_transaction()?;
        let last: i64 = tx.query_row(
            "SELECT backfill_row FROM quota_projection_meta WHERE id=1",
            [],
            |r| r.get(0),
        )?;
        let end: Option<i64> = tx.query_row("SELECT MAX(rowid) FROM (SELECT rowid FROM quota_metrics WHERE rowid>?1 ORDER BY rowid LIMIT 2000)", [last], |r| r.get(0))?;
        if let Some(end) = end {
            tx.execute("INSERT OR IGNORE INTO quota_sample_cache SELECT o.node_id,o.app_type,o.provider_id,m.metric_key,m.metric_kind,COALESCE(m.unit,''),o.sampled_at,o.observation_id,m.metric_label,m.unit,m.utilization_percent,m.used,m.remaining,m.total,m.resets_at FROM quota_metrics m JOIN quota_observations o ON o.node_id=m.node_id AND o.observation_id=m.observation_id WHERE m.rowid>?1 AND m.rowid<=?2", [last,end])?;
            tx.execute(
                "UPDATE quota_projection_meta SET backfill_row=?1 WHERE id=1",
                [end],
            )?;
        } else {
            tx.execute("UPDATE quota_projection_meta SET ready=1 WHERE id=1", [])?;
        }
        tx.commit()?;
    }
    Ok(())
}

pub(crate) fn next_dirty(db: &Connection) -> rusqlite::Result<Option<Vec<String>>> {
    db.query_row("SELECT node_id,app_type,provider_id,metric_key,metric_kind,unit_key FROM quota_reset_cache WHERE cached_revision!=revision LIMIT 1", [], |r| (0..6).map(|i| r.get(i)).collect()).optional()
}
