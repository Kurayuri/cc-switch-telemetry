use rusqlite::{params, Connection, OptionalExtension};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub(crate) const HOUR_SECONDS: i64 = 3_600;

pub(crate) fn hour_start(timestamp: i64) -> i64 {
    timestamp.div_euclid(HOUR_SECONDS) * HOUR_SECONDS
}

pub(crate) fn ensure_schema(connection: &Connection) -> anyhow::Result<()> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS usage_cache_partitions (
           node_id TEXT NOT NULL,
           hour_start INTEGER NOT NULL,
           state TEXT NOT NULL CHECK(state IN ('dirty','clean')),
           updated_at INTEGER NOT NULL,
           PRIMARY KEY (node_id,hour_start)
         );
         CREATE INDEX IF NOT EXISTS idx_usage_cache_partitions_state
           ON usage_cache_partitions(state,hour_start,node_id);
         CREATE TABLE IF NOT EXISTS usage_hourly_cache (
           node_id TEXT NOT NULL,
           hour_start INTEGER NOT NULL,
           app_type TEXT NOT NULL,
           provider_app_type TEXT NOT NULL,
           provider_id TEXT NOT NULL,
           model TEXT NOT NULL,
           request_model TEXT NOT NULL,
           pricing_model TEXT NOT NULL,
           data_source TEXT NOT NULL,
           request_count INTEGER NOT NULL,
           success_count INTEGER NOT NULL,
           input_tokens INTEGER NOT NULL,
           output_tokens INTEGER NOT NULL,
           cache_read_tokens INTEGER NOT NULL,
           cache_creation_tokens INTEGER NOT NULL,
           total_cost_usd TEXT NOT NULL,
           latency_total_ms REAL NOT NULL,
           first_event_at INTEGER NOT NULL,
           last_event_at INTEGER NOT NULL,
           PRIMARY KEY (
             node_id,hour_start,app_type,provider_app_type,provider_id,model,
             request_model,pricing_model,data_source
           )
         );
         CREATE INDEX IF NOT EXISTS idx_usage_hourly_cache_time
           ON usage_hourly_cache(hour_start,node_id);
         CREATE INDEX IF NOT EXISTS idx_usage_hourly_cache_filters
           ON usage_hourly_cache(node_id,app_type,provider_id,model,data_source,hour_start);",
    )?;
    connection.execute(
        "INSERT INTO usage_cache_partitions(node_id,hour_start,state,updated_at)
         SELECT node_id,created_at - ((created_at % 3600 + 3600) % 3600),'dirty',0
         FROM usage_events
         GROUP BY node_id,created_at - ((created_at % 3600 + 3600) % 3600)
         ON CONFLICT(node_id,hour_start) DO NOTHING",
        [],
    )?;
    Ok(())
}

pub(crate) fn mark_event_dirty(
    connection: &Connection,
    node_id: &str,
    created_at: i64,
) -> rusqlite::Result<()> {
    let hour = hour_start(created_at);
    let now = chrono::Utc::now().timestamp();
    connection.execute(
        "INSERT INTO usage_cache_partitions(node_id,hour_start,state,updated_at)
         VALUES (?1,?2,'dirty',?3)
         ON CONFLICT(node_id,hour_start) DO UPDATE SET
           state='dirty',updated_at=excluded.updated_at",
        params![node_id, hour, now],
    )?;
    connection.execute(
        "DELETE FROM usage_hourly_cache WHERE node_id=?1 AND hour_start=?2",
        params![node_id, hour],
    )?;
    Ok(())
}

pub(crate) fn mark_range_dirty(
    connection: &Connection,
    node_id: &str,
    from: i64,
    to: i64,
) -> rusqlite::Result<()> {
    let now = chrono::Utc::now().timestamp();
    connection.execute(
        "INSERT INTO usage_cache_partitions(node_id,hour_start,state,updated_at)
         SELECT node_id,created_at - ((created_at % 3600 + 3600) % 3600),'dirty',?4
         FROM usage_events
         WHERE node_id=?1 AND created_at>=?2 AND created_at<?3
         GROUP BY node_id,created_at - ((created_at % 3600 + 3600) % 3600)
         ON CONFLICT(node_id,hour_start) DO UPDATE SET
           state='dirty',updated_at=excluded.updated_at",
        params![node_id, from, to, now],
    )?;
    connection.execute(
        "DELETE FROM usage_hourly_cache
         WHERE node_id=?1 AND hour_start>=?2 AND hour_start<?3",
        params![node_id, hour_start(from), to],
    )?;
    Ok(())
}

pub(crate) fn mark_node_dirty(connection: &Connection, node_id: &str) -> rusqlite::Result<()> {
    let now = chrono::Utc::now().timestamp();
    connection.execute(
        "INSERT INTO usage_cache_partitions(node_id,hour_start,state,updated_at)
         SELECT node_id,created_at - ((created_at % 3600 + 3600) % 3600),'dirty',?2
         FROM usage_events WHERE node_id=?1
         GROUP BY node_id,created_at - ((created_at % 3600 + 3600) % 3600)
         ON CONFLICT(node_id,hour_start) DO UPDATE SET
           state='dirty',updated_at=excluded.updated_at",
        params![node_id, now],
    )?;
    connection.execute("DELETE FROM usage_hourly_cache WHERE node_id=?1", [node_id])?;
    Ok(())
}

fn rebuild_one_before(connection: &Connection, before_hour: Option<i64>) -> anyhow::Result<bool> {
    let partition = match before_hour {
        Some(before_hour) => connection
            .query_row(
                "SELECT node_id,hour_start FROM usage_cache_partitions
                 WHERE state='dirty' AND hour_start<?1
                 ORDER BY updated_at,hour_start,node_id LIMIT 1",
                [before_hour],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()?,
        None => connection
            .query_row(
                "SELECT node_id,hour_start FROM usage_cache_partitions
                 WHERE state='dirty' ORDER BY updated_at,hour_start,node_id LIMIT 1",
                [],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()?,
    };
    let Some((node_id, start)) = partition else {
        return Ok(false);
    };
    let end = start + HOUR_SECONDS;
    let fresh = cc_switch_usage_core::sql::fresh_input("e");
    let app = cc_switch_usage_core::sql::folded_app_type("e.app_type");
    let model = cc_switch_usage_core::sql::effective_model("e");
    let sql = format!(
        "INSERT INTO usage_hourly_cache (
           node_id,hour_start,app_type,provider_app_type,provider_id,model,
           request_model,pricing_model,data_source,request_count,success_count,
           input_tokens,output_tokens,cache_read_tokens,cache_creation_tokens,
           total_cost_usd,latency_total_ms,first_event_at,last_event_at
         )
         SELECT e.node_id,?2,{app},e.app_type,e.provider_id,{model},
                e.request_model,e.pricing_model,e.data_source,
                COUNT(*),
                SUM(CASE WHEN e.status_code>=200 AND e.status_code<300 THEN 1 ELSE 0 END),
                COALESCE(SUM({fresh}),0),COALESCE(SUM(e.output_tokens),0),
                COALESCE(SUM(e.cache_read_tokens),0),
                COALESCE(SUM(e.cache_creation_tokens),0),
                CAST(COALESCE(SUM(CAST(e.total_cost_usd AS REAL)),0) AS TEXT),
                COALESCE(SUM(e.latency_ms),0),MIN(e.created_at),MAX(e.created_at)
         FROM usage_events e
         WHERE e.node_id=?1 AND e.created_at>=?2 AND e.created_at<?3
         GROUP BY e.node_id,{app},e.app_type,e.provider_id,{model},
                  e.request_model,e.pricing_model,e.data_source"
    );
    let transaction = connection.unchecked_transaction()?;
    transaction.execute(
        "DELETE FROM usage_hourly_cache WHERE node_id=?1 AND hour_start=?2",
        params![node_id, start],
    )?;
    transaction.execute(&sql, params![node_id, start, end])?;
    transaction.execute(
        "UPDATE usage_cache_partitions SET state='clean',updated_at=?3
         WHERE node_id=?1 AND hour_start=?2",
        params![node_id, start, chrono::Utc::now().timestamp()],
    )?;
    transaction.commit()?;
    Ok(true)
}

#[cfg(test)]
pub(crate) fn rebuild_one(connection: &Connection) -> anyhow::Result<bool> {
    rebuild_one_before(connection, None)
}

pub(crate) fn spawn_worker(db: Arc<Mutex<Connection>>) {
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        return;
    };
    handle.spawn(async move {
        loop {
            let rebuilt = match db.try_lock() {
                Ok(connection) => match rebuild_one_before(
                    &connection,
                    Some(hour_start(chrono::Utc::now().timestamp())),
                ) {
                    Ok(value) => value,
                    Err(error) => {
                        eprintln!("usage cache rebuild failed: {error}");
                        false
                    }
                },
                Err(_) => false,
            };
            tokio::time::sleep(if rebuilt {
                Duration::from_millis(100)
            } else {
                Duration::from_secs(1)
            })
            .await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn negative_timestamps_use_floor_hours() {
        assert_eq!(hour_start(-1), -3_600);
        assert_eq!(hour_start(3_601), 3_600);
    }
}
