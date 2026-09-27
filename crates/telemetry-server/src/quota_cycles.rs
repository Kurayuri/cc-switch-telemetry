//! Exact cycle summaries: chart downsampling must not hide a full-quota sample.
use crate::ServerState;
use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use rusqlite::{params, Connection, OpenFlags};
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CycleQuery {
    node_id: String,
    provider_id: String,
    metric_key: String,
    metric_kind: String,
    unit: Option<String>,
    cycles: Vec<CycleBounds>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CycleBounds {
    from: i64,
    to: i64,
    resets_at: i64,
}

impl CycleQuery {
    fn valid(&self, now: i64) -> bool {
        [
            &self.node_id,
            &self.provider_id,
            &self.metric_key,
            &self.metric_kind,
        ]
        .iter()
        .all(|value| {
            !value.trim().is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
        }) && self
            .unit
            .as_ref()
            .is_none_or(|unit| unit.len() <= 256 && !unit.chars().any(char::is_control))
            && !self.cycles.is_empty()
            && self.cycles.len() <= 128
            && self.cycles.iter().all(|cycle| {
                cycle.from >= 0
                    && cycle.from < cycle.to
                    && cycle.to <= now
                    && cycle.to <= cycle.resets_at
                    && cycle.resets_at - cycle.from <= 720 * 86400
            })
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CycleSummary {
    #[serde(flatten)]
    bounds: CycleBounds,
    reached_full: bool,
    sampled_at: Option<i64>,
    utilization_percent: Option<f64>,
}

fn query_summaries(
    path: &std::path::Path,
    query: &CycleQuery,
) -> anyhow::Result<Vec<CycleSummary>> {
    let mut connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    connection.busy_timeout(std::time::Duration::from_secs(5))?;
    let transaction = connection.transaction()?;
    let mut statement = transaction.prepare(
        "SELECT sampled_at,utilization_percent,used,remaining,total FROM quota_sample_cache
         WHERE node_id=?1 AND app_type='codex' AND provider_id=?2 AND metric_key=?3
           AND metric_kind=?4 AND unit_key=?5 AND sampled_at>=?6 AND sampled_at<?7
           AND ABS(resets_at-?8)<=60 ORDER BY sampled_at,observation_id",
    )?;
    let mut summaries = Vec::with_capacity(query.cycles.len());
    for cycle in &query.cycles {
        let mut summary = CycleSummary {
            bounds: CycleBounds {
                from: cycle.from,
                to: cycle.to,
                resets_at: cycle.resets_at,
            },
            reached_full: false,
            sampled_at: None,
            utilization_percent: None,
        };
        let mut rows = statement.query(params![
            query.node_id,
            query.provider_id,
            query.metric_key,
            query.metric_kind,
            query.unit.as_deref().unwrap_or(""),
            cycle.from,
            cycle.to,
            cycle.resets_at
        ])?;
        while let Some(row) = rows.next()? {
            let percent: Option<f64> = row.get(1)?;
            let used: Option<f64> = row.get(2)?;
            let remaining: Option<f64> = row.get(3)?;
            let total: Option<f64> = row.get(4)?;
            let percent = percent.filter(|value| value.is_finite()).or_else(|| {
                total
                    .filter(|value| value.is_finite() && *value > 0.0)
                    .and_then(|total| {
                        used.filter(|value| value.is_finite())
                            .map(|used| used / total * 100.0)
                            .or_else(|| {
                                remaining
                                    .filter(|value| value.is_finite())
                                    .map(|remaining| (total - remaining) / total * 100.0)
                            })
                    })
            });
            if let Some(percent) = percent.filter(|value| value.is_finite() && *value > 0.0) {
                summary.reached_full |= percent >= 100.0;
                summary.sampled_at = Some(row.get(0)?);
                summary.utilization_percent = Some(percent.clamp(0.0, 100.0));
            }
        }
        summaries.push(summary);
    }
    Ok(summaries)
}

fn error(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(serde_json::json!({"code": code, "message": message})),
    )
        .into_response()
}

pub async fn summaries(
    State(state): State<ServerState>,
    Json(query): Json<CycleQuery>,
) -> Response {
    if !query.valid(chrono::Utc::now().timestamp()) {
        return error(
            StatusCode::BAD_REQUEST,
            "invalid_query",
            "Provide a complete quota identity and 1–128 valid ended cycles",
        );
    }
    match tokio::task::spawn_blocking(move || query_summaries(&state.db_path, &query)).await {
        Ok(Ok(cycles)) => Json(serde_json::json!({"cycles": cycles})).into_response(),
        Ok(Err(err)) => error(
            StatusCode::SERVICE_UNAVAILABLE,
            "query_failed",
            &err.to_string(),
        ),
        Err(err) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "query_failed",
            &err.to_string(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, extract::connect_info::MockConnectInfo, http::Request};
    use std::net::SocketAddr;
    use tower::ServiceExt;

    fn query(node: &str) -> CycleQuery {
        serde_json::from_value(serde_json::json!({"nodeId":node,"providerId":"quota",
            "metricKey":"5h","metricKind":"utilizationPercent","unit":"%",
            "cycles":[{"from":100,"to":200,"resetsAt":300}]}))
        .unwrap()
    }

    #[test]
    fn raw_peak_last_valid_sample_and_early_reset_are_scoped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.db");
        let db = crate::init_db(&path).unwrap();
        let (node, _) = crate::nodes::create(&db, "node").unwrap();
        let node = node.uuid;
        // Peak lies between bucket head/tail. Last sample is zero; wrong-reset
        // samples and early-reset boundary must not contribute to this cycle.
        for (at, percent, reset) in [
            (110, 20., 300),
            (120, 100., 300),
            (130, 50., 300),
            (140, 0., 300),
            (150, 99., 500),
            (200, 100., 300),
        ] {
            db.execute("INSERT INTO quota_sample_cache VALUES (?1,'codex','quota','5h','utilizationPercent','%',?2,?3,'5h','%',?4,NULL,NULL,NULL,?5)",
                params![node,at,format!("sample-{at}"),percent,reset]).unwrap();
        }
        let result = query_summaries(&path, &query(&node)).unwrap();
        assert!(result[0].reached_full);
        assert_eq!(result[0].sampled_at, Some(130));
        assert_eq!(result[0].utilization_percent, Some(50.));
        let mut other = query(&node);
        other.metric_kind = "balance".into();
        assert_eq!(query_summaries(&path, &other).unwrap()[0].sampled_at, None);
        db.execute("DELETE FROM quota_sample_cache WHERE sampled_at=120", [])
            .unwrap();
        assert!(!query_summaries(&path, &query(&node)).unwrap()[0].reached_full);
        // Fallback matches UI precedence, including explicit zero overriding used.
        db.execute("UPDATE quota_sample_cache SET utilization_percent=NULL,used=3,total=4 WHERE sampled_at=130", []).unwrap();
        assert_eq!(
            query_summaries(&path, &query(&node)).unwrap()[0].utilization_percent,
            Some(75.)
        );
        db.execute(
            "UPDATE quota_sample_cache SET used=NULL,remaining=1,total=4 WHERE sampled_at=130",
            [],
        )
        .unwrap();
        assert_eq!(
            query_summaries(&path, &query(&node)).unwrap()[0].utilization_percent,
            Some(75.)
        );
    }

    #[tokio::test]
    async fn validates_bounds_batch_size_and_dashboard_access() {
        let mut q = query("node");
        assert!(q.valid(200));
        assert!(!q.valid(199));
        q.cycles[0].from = -1;
        assert!(!q.valid(200));
        q.cycles[0].from = 200;
        assert!(!q.valid(200));
        q.cycles.clear();
        assert!(!q.valid(200));
        q.cycles = (0..129)
            .map(|_| CycleBounds {
                from: 100,
                to: 200,
                resets_at: 300,
            })
            .collect();
        assert!(!q.valid(200));
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.db");
        let db = crate::init_db(&path).unwrap();
        let state = ServerState::new(db, path, None);
        for (address, expected) in [
            ([127, 0, 0, 1], StatusCode::OK),
            ([192, 0, 2, 1], StatusCode::FORBIDDEN),
        ] {
            let response = crate::router(state.clone())
                .layer(MockConnectInfo(SocketAddr::from((address,12345))))
                .oneshot(Request::builder().method("POST").uri("/v3/dashboard/quota/cycle-summaries")
                    .header("content-type","application/json")
                    .body(Body::from(r#"{"nodeId":"node","providerId":"quota","metricKey":"5h","metricKind":"utilizationPercent","unit":"%","cycles":[{"from":100,"to":200,"resetsAt":300}]}"#)).unwrap()).await.unwrap();
            assert_eq!(response.status(), expected);
        }
    }
}
