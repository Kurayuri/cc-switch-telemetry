//! Exact per-event billing projections, versioned by effective pricing settings.
use crate::settings::ModelBillingMultiplier;
use rusqlite::{Connection, OptionalExtension};
use sha2::{Digest, Sha256};

pub(crate) fn ensure_schema(db: &Connection) -> anyhow::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS usage_billing_cache (
      event_id TEXT PRIMARY KEY REFERENCES usage_events(event_id) ON DELETE CASCADE,
      signature TEXT NOT NULL,cost REAL NOT NULL,unadjusted INTEGER NOT NULL);
      CREATE TRIGGER IF NOT EXISTS usage_billing_invalidate AFTER UPDATE ON usage_events BEGIN
        DELETE FROM usage_billing_cache WHERE event_id=OLD.event_id;
      END;
      CREATE TRIGGER IF NOT EXISTS usage_billing_delete AFTER DELETE ON usage_events BEGIN
        DELETE FROM usage_billing_cache WHERE event_id=OLD.event_id;
      END;
      CREATE TABLE IF NOT EXISTS billing_projection_meta(id INTEGER PRIMARY KEY,revision INTEGER NOT NULL,processed INTEGER NOT NULL,signature TEXT NOT NULL);
      INSERT OR IGNORE INTO billing_projection_meta VALUES(1,0,-1,'');
      CREATE TRIGGER IF NOT EXISTS billing_revision_insert AFTER INSERT ON usage_events BEGIN
        UPDATE billing_projection_meta SET revision=revision+1 WHERE id=1;
      END;
      CREATE TRIGGER IF NOT EXISTS billing_revision_update AFTER UPDATE ON usage_events BEGIN
        UPDATE billing_projection_meta SET revision=revision+1 WHERE id=1;
      END;
      CREATE TRIGGER IF NOT EXISTS billing_revision_delete AFTER DELETE ON usage_events BEGIN
        UPDATE billing_projection_meta SET revision=revision+1 WHERE id=1;
      END;")?;
    let columns = db
        .prepare("PRAGMA table_info(usage_hourly_cache)")?
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    for (name, declaration) in [
        ("billing_signature", "TEXT NOT NULL DEFAULT 'base'"),
        ("adjusted_cost", "REAL"),
        ("unadjusted_requests", "INTEGER NOT NULL DEFAULT 0"),
    ] {
        if !columns.iter().any(|c| c == name) {
            db.execute_batch(&format!(
                "ALTER TABLE usage_hourly_cache ADD COLUMN {name} {declaration}"
            ))?;
        }
    }
    Ok(())
}

pub(crate) fn signature(entry: &ModelBillingMultiplier) -> String {
    let value = serde_json::json!([
        1,
        entry.model,
        entry.factors(),
        entry.reference_pricing.as_ref().map(|p| p.prices())
    ]);
    format!("{:x}", Sha256::digest(value.to_string().as_bytes()))
}

pub(crate) fn signature_sql(model: &str, entries: &[ModelBillingMultiplier]) -> String {
    let cases = entries
        .iter()
        .filter(|e| e.component_adjustment())
        .map(|e| {
            format!(
                "WHEN {model}={} THEN '{}'",
                crate::dashboard::sql_string_literal(&e.model),
                signature(e)
            )
        })
        .collect::<Vec<_>>();
    if cases.is_empty() {
        "'base'".into()
    } else {
        format!("CASE {} ELSE 'base' END", cases.join(" "))
    }
}

pub(crate) fn projection_sql(
    alias: &str,
    model: &str,
    entries: &[ModelBillingMultiplier],
) -> (String, String) {
    let (cost, unadjusted) = crate::dashboard::billing_projection_sql(alias, model, entries);
    if !entries.iter().any(|e| e.component_adjustment()) {
        return (cost, unadjusted);
    }
    let signature = signature_sql(model, entries);
    (format!("COALESCE((SELECT b.cost FROM usage_billing_cache b WHERE b.event_id={alias}.event_id AND b.signature={signature}),{cost})"),
     format!("COALESCE((SELECT b.unadjusted FROM usage_billing_cache b WHERE b.event_id={alias}.event_id AND b.signature={signature}),{unadjusted})"))
}

pub(crate) fn rebuild_batch(
    db: &Connection,
    entries: &[ModelBillingMultiplier],
) -> anyhow::Result<bool> {
    let key = signature_sql("model", entries);
    let (revision, processed, previous): (i64, i64, String) = db.query_row(
        "SELECT revision,processed,signature FROM billing_projection_meta WHERE id=1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    if revision == processed && previous == key {
        return Ok(false);
    }
    let model = cc_switch_usage_core::sql::effective_model("e");
    let signature = signature_sql(&model, entries);
    let (cost, unadjusted) = crate::dashboard::billing_projection_sql("e", &model, entries);
    let changed=db.execute(&format!("INSERT OR REPLACE INTO usage_billing_cache(event_id,signature,cost,unadjusted)
        SELECT e.event_id,{signature},{cost},{unadjusted} FROM usage_events e
        WHERE {signature}!='base' AND NOT EXISTS(SELECT 1 FROM usage_billing_cache b WHERE b.event_id=e.event_id AND b.signature={signature}) LIMIT 2000"),[])?;
    // Existing clean partitions can have a stale billing snapshot. Rebuild one
    // bounded partition at a time; reads fall back to raw until it is ready.
    let expected = signature_sql("c.model", entries);
    let stale: Option<(String, i64)> = db
        .query_row(
            &format!(
                "SELECT c.node_id,c.hour_start FROM usage_hourly_cache c
        JOIN usage_cache_partitions p ON p.node_id=c.node_id AND p.hour_start=c.hour_start
        WHERE p.state='clean' AND c.billing_signature!={expected} ORDER BY c.hour_start DESC LIMIT 1"
            ),
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    if let Some((node, hour)) = &stale {
        db.execute(
            "UPDATE usage_cache_partitions SET state='dirty' WHERE node_id=?1 AND hour_start=?2",
            rusqlite::params![node, hour],
        )?;
    }
    if changed == 0 && stale.is_none() {
        db.execute(
            "UPDATE billing_projection_meta SET processed=?1,signature=?2 WHERE id=1",
            rusqlite::params![revision, key],
        )?;
    }
    Ok(changed > 0 || stale.is_some())
}

pub(crate) fn valid_hour_sql(node: &str, hour: &str, entries: &[ModelBillingMultiplier]) -> String {
    let expected = signature_sql("bc.model", entries);
    format!("NOT EXISTS(SELECT 1 FROM usage_hourly_cache bc WHERE bc.node_id={node} AND bc.hour_start={hour} AND bc.billing_signature!={expected})")
}
