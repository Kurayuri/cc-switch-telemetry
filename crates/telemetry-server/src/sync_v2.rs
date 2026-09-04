use crate::{authenticated_node, usage_cache, ServerState};
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use rusqlite::{params, OptionalExtension, Transaction};
use serde::Serialize;
use sha2::{Digest, Sha256};
use telemetry_core::{
    event_id, rollup_key, EventMutation, EventMutationBatch, MutationKind, ProviderEntry,
    ProviderMutationBatch, RollupMutation, RollupMutationBatch, RollupSnapshot, SyncBeginRequest,
    SyncBeginResponse, SyncCommitRequest, SyncCommitResponse, UsageEvent, SCHEMA_VERSION,
};

const MAX_BATCH_ITEMS: usize = 2_000;

fn error(status: StatusCode, message: &str) -> Response {
    (status, Json(serde_json::json!({ "error": message }))).into_response()
}

fn valid_schema(schema_version: u32) -> bool {
    schema_version == SCHEMA_VERSION
}

fn content_hash<T: Serialize>(value: &T) -> Result<String, serde_json::Error> {
    let bytes = serde_json::to_vec(value)?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn authenticated(headers: &HeaderMap, state: &ServerState) -> Option<String> {
    authenticated_node(headers, &state.db)
}

fn generation_is_open(
    connection: &rusqlite::Connection,
    generation_id: &str,
    node_id: &str,
) -> rusqlite::Result<bool> {
    connection.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM sync_generations
             WHERE generation_id=?1 AND node_id=?2 AND status='open'
         )",
        params![generation_id, node_id],
        |row| row.get(0),
    )
}

pub async fn begin(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Json(request): Json<SyncBeginRequest>,
) -> Response {
    if !valid_schema(request.schema_version) {
        return error(
            StatusCode::UPGRADE_REQUIRED,
            "telemetry protocol v3 is required",
        );
    }
    let Some(node_id) = authenticated(&headers, &state) else {
        return error(StatusCode::UNAUTHORIZED, "invalid node token");
    };
    if request.generation_id.trim().is_empty() {
        return error(StatusCode::BAD_REQUEST, "invalid generation metadata");
    }
    let Ok(connection) = state.db.lock() else {
        return error(StatusCode::INTERNAL_SERVER_ERROR, "database unavailable");
    };
    let existing = connection
        .query_row(
            "SELECT node_id,replace_all,status FROM sync_generations
             WHERE generation_id=?1",
            [&request.generation_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, bool>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional();
    let existing = match existing {
        Ok(value) => value,
        Err(_) => return error(StatusCode::INTERNAL_SERVER_ERROR, "database query failed"),
    };
    if let Some((owner, replace_all, status)) = existing {
        if owner != node_id || replace_all != request.replace_all || status != "open" {
            return error(StatusCode::CONFLICT, "generation cannot be resumed");
        }
        return (
            StatusCode::OK,
            Json(SyncBeginResponse {
                generation_id: request.generation_id,
                resumed: true,
            }),
        )
            .into_response();
    }
    if connection
        .execute(
            "INSERT INTO sync_generations
             (generation_id,node_id,replace_all,status,created_at)
             VALUES (?1,?2,?3,'open',?4)",
            params![
                request.generation_id,
                node_id,
                request.replace_all,
                chrono::Utc::now().timestamp()
            ],
        )
        .is_err()
    {
        return error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "create generation failed",
        );
    }
    (
        StatusCode::CREATED,
        Json(SyncBeginResponse {
            generation_id: request.generation_id,
            resumed: false,
        }),
    )
        .into_response()
}

fn validate_event_mutation(mutation: &EventMutation) -> bool {
    if mutation.app_type.trim().is_empty()
        || mutation.request_id.trim().is_empty()
        || mutation.content_hash.trim().is_empty()
    {
        return false;
    }
    match (&mutation.operation, &mutation.event) {
        (MutationKind::Upsert, Some(event)) => {
            event.app_type == mutation.app_type
                && event.request_id == mutation.request_id
                && !event.app_type.trim().is_empty()
                && !event.model.trim().is_empty()
        }
        (MutationKind::Delete, None) => true,
        _ => false,
    }
}

pub async fn stage_events(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Json(batch): Json<EventMutationBatch>,
) -> Response {
    if !valid_schema(batch.schema_version) {
        return error(
            StatusCode::UPGRADE_REQUIRED,
            "telemetry protocol v3 is required",
        );
    }
    let Some(node_id) = authenticated(&headers, &state) else {
        return error(StatusCode::UNAUTHORIZED, "invalid node token");
    };
    if batch.mutations.len() > MAX_BATCH_ITEMS
        || batch
            .mutations
            .iter()
            .any(|item| !validate_event_mutation(item))
    {
        return error(StatusCode::BAD_REQUEST, "invalid event mutation batch");
    }
    let Ok(mut connection) = state.db.lock() else {
        return error(StatusCode::INTERNAL_SERVER_ERROR, "database unavailable");
    };
    if !generation_is_open(&connection, &batch.generation_id, &node_id).unwrap_or(false) {
        return error(StatusCode::CONFLICT, "generation is not open");
    }
    let Ok(transaction) = connection.transaction() else {
        return error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "start staging transaction failed",
        );
    };
    for mutation in batch.mutations {
        let payload = match mutation.event {
            Some(event) => match serde_json::to_string(&event) {
                Ok(value) => Some(value),
                Err(_) => return error(StatusCode::BAD_REQUEST, "encode event failed"),
            },
            None => None,
        };
        if transaction
            .execute(
                "INSERT INTO staged_event_mutations
                 (generation_id,app_type,request_id,operation,content_hash,payload_json)
                 VALUES (?1,?2,?3,?4,?5,?6)
                 ON CONFLICT(generation_id,app_type,request_id) DO UPDATE SET
                    operation=excluded.operation,
                    content_hash=excluded.content_hash,
                    payload_json=excluded.payload_json",
                params![
                    batch.generation_id,
                    mutation.app_type,
                    mutation.request_id,
                    match mutation.operation {
                        MutationKind::Upsert => "upsert",
                        MutationKind::Delete => "delete",
                    },
                    mutation.content_hash,
                    payload
                ],
            )
            .is_err()
        {
            return error(StatusCode::INTERNAL_SERVER_ERROR, "stage event failed");
        }
    }
    match transaction.commit() {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(_) => error(StatusCode::INTERNAL_SERVER_ERROR, "commit staging failed"),
    }
}

fn validate_rollup_mutation(mutation: &RollupMutation) -> bool {
    if mutation.snapshot_key.trim().is_empty() || mutation.content_hash.trim().is_empty() {
        return false;
    }
    match (&mutation.operation, &mutation.snapshot) {
        (MutationKind::Upsert, Some(snapshot)) => {
            snapshot.schema_version == SCHEMA_VERSION
                && snapshot.input_token_semantics
                    == cc_switch_usage_core::INPUT_TOKEN_SEMANTICS_FRESH
                && snapshot.day_start_utc < snapshot.day_end_utc
                && snapshot.avg_latency_ms.is_finite()
                && snapshot.avg_latency_ms >= 0.0
                && !snapshot.date.trim().is_empty()
        }
        (MutationKind::Delete, None) => true,
        _ => false,
    }
}

pub async fn stage_rollups(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Json(batch): Json<RollupMutationBatch>,
) -> Response {
    if !valid_schema(batch.schema_version) {
        return error(
            StatusCode::UPGRADE_REQUIRED,
            "telemetry protocol v3 is required",
        );
    }
    let Some(node_id) = authenticated(&headers, &state) else {
        return error(StatusCode::UNAUTHORIZED, "invalid node token");
    };
    if batch.mutations.len() > MAX_BATCH_ITEMS
        || batch
            .mutations
            .iter()
            .any(|item| !validate_rollup_mutation(item))
    {
        return error(StatusCode::BAD_REQUEST, "invalid rollup mutation batch");
    }
    let Ok(mut connection) = state.db.lock() else {
        return error(StatusCode::INTERNAL_SERVER_ERROR, "database unavailable");
    };
    if !generation_is_open(&connection, &batch.generation_id, &node_id).unwrap_or(false) {
        return error(StatusCode::CONFLICT, "generation is not open");
    }
    let Ok(transaction) = connection.transaction() else {
        return error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "start staging transaction failed",
        );
    };
    for mutation in batch.mutations {
        let payload = match mutation.snapshot {
            Some(snapshot) => match serde_json::to_string(&snapshot) {
                Ok(value) => Some(value),
                Err(_) => return error(StatusCode::BAD_REQUEST, "encode rollup failed"),
            },
            None => None,
        };
        if transaction
            .execute(
                "INSERT INTO staged_rollup_mutations
                 (generation_id,snapshot_key,operation,content_hash,payload_json)
                 VALUES (?1,?2,?3,?4,?5)
                 ON CONFLICT(generation_id,snapshot_key) DO UPDATE SET
                    operation=excluded.operation,
                    content_hash=excluded.content_hash,
                    payload_json=excluded.payload_json",
                params![
                    batch.generation_id,
                    mutation.snapshot_key,
                    match mutation.operation {
                        MutationKind::Upsert => "upsert",
                        MutationKind::Delete => "delete",
                    },
                    mutation.content_hash,
                    payload
                ],
            )
            .is_err()
        {
            return error(StatusCode::INTERNAL_SERVER_ERROR, "stage rollup failed");
        }
    }
    match transaction.commit() {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(_) => error(StatusCode::INTERNAL_SERVER_ERROR, "commit staging failed"),
    }
}

pub async fn stage_providers(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Json(batch): Json<ProviderMutationBatch>,
) -> Response {
    if !valid_schema(batch.schema_version) {
        return error(
            StatusCode::UPGRADE_REQUIRED,
            "telemetry protocol v3 is required",
        );
    }
    let Some(node_id) = authenticated(&headers, &state) else {
        return error(StatusCode::UNAUTHORIZED, "invalid node token");
    };
    if batch.providers.len() > 10_000
        || batch.providers.iter().any(|provider| {
            provider.app_type.trim().is_empty()
                || provider.provider_id.trim().is_empty()
                || provider.name.trim().is_empty()
        })
    {
        return error(StatusCode::BAD_REQUEST, "invalid provider batch");
    }
    let Ok(mut connection) = state.db.lock() else {
        return error(StatusCode::INTERNAL_SERVER_ERROR, "database unavailable");
    };
    if !generation_is_open(&connection, &batch.generation_id, &node_id).unwrap_or(false) {
        return error(StatusCode::CONFLICT, "generation is not open");
    }
    let Ok(transaction) = connection.transaction() else {
        return error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "start staging transaction failed",
        );
    };
    for provider in batch.providers {
        if transaction
            .execute(
                "INSERT INTO staged_providers (generation_id,app_type,provider_id,name)
                 VALUES (?1,?2,?3,?4)
                 ON CONFLICT(generation_id,app_type,provider_id) DO UPDATE SET
                    name=excluded.name",
                params![
                    batch.generation_id,
                    provider.app_type,
                    provider.provider_id,
                    provider.name
                ],
            )
            .is_err()
        {
            return error(StatusCode::INTERNAL_SERVER_ERROR, "stage provider failed");
        }
    }
    match transaction.commit() {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(_) => error(StatusCode::INTERNAL_SERVER_ERROR, "commit staging failed"),
    }
}

fn upsert_event(
    transaction: &Transaction<'_>,
    node_id: &str,
    event: &UsageEvent,
    content_hash: &str,
    result: &mut SyncCommitResponse,
) -> rusqlite::Result<()> {
    let existing = transaction
        .query_row(
            "SELECT content_hash,created_at FROM usage_events
             WHERE node_id=?1 AND app_type=?2 AND request_id=?3",
            params![node_id, event.app_type, event.request_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
        )
        .optional()?;
    if existing
        .as_ref()
        .is_some_and(|(hash, _)| hash == content_hash)
    {
        result.unchanged += 1;
        return Ok(());
    }
    if let Some((_, created_at)) = &existing {
        usage_cache::mark_event_dirty(transaction, node_id, *created_at)?;
    }
    usage_cache::mark_event_dirty(transaction, node_id, event.created_at)?;
    transaction.execute(
        "INSERT INTO usage_events (
             event_id,node_id,request_id,created_at,app_type,provider_id,model,
             request_model,pricing_model,input_tokens,output_tokens,cache_read_tokens,
             cache_creation_tokens,input_token_semantics,total_cost_usd,latency_ms,
             status_code,is_streaming,data_source,content_hash,received_at
         ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21)
         ON CONFLICT(node_id,app_type,request_id) DO UPDATE SET
             event_id=excluded.event_id,
             created_at=excluded.created_at,
             app_type=excluded.app_type,
             provider_id=excluded.provider_id,
             model=excluded.model,
             request_model=excluded.request_model,
             pricing_model=excluded.pricing_model,
             input_tokens=excluded.input_tokens,
             output_tokens=excluded.output_tokens,
             cache_read_tokens=excluded.cache_read_tokens,
             cache_creation_tokens=excluded.cache_creation_tokens,
             input_token_semantics=excluded.input_token_semantics,
             total_cost_usd=excluded.total_cost_usd,
             latency_ms=excluded.latency_ms,
             status_code=excluded.status_code,
             is_streaming=excluded.is_streaming,
             data_source=excluded.data_source,
             content_hash=excluded.content_hash,
             received_at=excluded.received_at",
        params![
            event_id(node_id, &event.app_type, &event.request_id),
            node_id,
            event.request_id,
            event.created_at,
            event.app_type,
            event.provider_id,
            event.model,
            event.request_model.clone().unwrap_or_default(),
            event.pricing_model.clone().unwrap_or_default(),
            event.input_tokens,
            event.output_tokens,
            event.cache_read_tokens,
            event.cache_creation_tokens,
            event.input_token_semantics,
            event.total_cost_usd,
            event.latency_ms,
            event.status_code,
            event.is_streaming as i64,
            event.data_source,
            content_hash,
            chrono::Utc::now().timestamp()
        ],
    )?;
    if existing.is_some() {
        result.updated += 1;
    } else {
        result.inserted += 1;
    }
    Ok(())
}

fn upsert_rollup(
    transaction: &Transaction<'_>,
    node_id: &str,
    snapshot: &RollupSnapshot,
    content_hash: &str,
) -> rusqlite::Result<()> {
    let key = rollup_key(
        node_id,
        &snapshot.date,
        &snapshot.app_type,
        &snapshot.provider_id,
        &snapshot.model,
        &snapshot.request_model,
        &snapshot.pricing_model,
    );
    usage_cache::mark_range_dirty(
        transaction,
        node_id,
        snapshot.day_start_utc,
        snapshot.day_end_utc,
    )?;
    transaction.execute(
        "INSERT INTO usage_daily_snapshots (
             snapshot_key,node_id,date,app_type,provider_id,model,request_model,
             pricing_model,request_count,success_count,input_tokens,output_tokens,
             cache_read_tokens,cache_creation_tokens,input_token_semantics,total_cost_usd,
             avg_latency_ms,day_start_utc,day_end_utc,content_hash,received_at
         ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21)
         ON CONFLICT(snapshot_key) DO UPDATE SET
             request_count=excluded.request_count,
             success_count=excluded.success_count,
             input_tokens=excluded.input_tokens,
             output_tokens=excluded.output_tokens,
             cache_read_tokens=excluded.cache_read_tokens,
             cache_creation_tokens=excluded.cache_creation_tokens,
             input_token_semantics=excluded.input_token_semantics,
             total_cost_usd=excluded.total_cost_usd,
             avg_latency_ms=excluded.avg_latency_ms,
             day_start_utc=excluded.day_start_utc,
             day_end_utc=excluded.day_end_utc,
             content_hash=excluded.content_hash,
             received_at=excluded.received_at",
        params![
            key,
            node_id,
            snapshot.date,
            snapshot.app_type,
            snapshot.provider_id,
            snapshot.model,
            snapshot.request_model,
            snapshot.pricing_model,
            snapshot.request_count,
            snapshot.success_count,
            snapshot.input_tokens,
            snapshot.output_tokens,
            snapshot.cache_read_tokens,
            snapshot.cache_creation_tokens,
            snapshot.input_token_semantics,
            snapshot.total_cost_usd,
            snapshot.avg_latency_ms,
            snapshot.day_start_utc,
            snapshot.day_end_utc,
            content_hash,
            chrono::Utc::now().timestamp()
        ],
    )?;
    // Source rollups replace details for that complete source-local day. This
    // makes partial-day queries behave like cc-switch after local pruning.
    transaction.execute(
        "DELETE FROM usage_events
         WHERE node_id=?1 AND created_at>=?2 AND created_at<?3",
        params![node_id, snapshot.day_start_utc, snapshot.day_end_utc],
    )?;
    Ok(())
}

pub async fn commit(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Json(request): Json<SyncCommitRequest>,
) -> Response {
    if !valid_schema(request.schema_version) {
        return error(
            StatusCode::UPGRADE_REQUIRED,
            "telemetry protocol v3 is required",
        );
    }
    let Some(node_id) = authenticated(&headers, &state) else {
        return error(StatusCode::UNAUTHORIZED, "invalid node token");
    };
    if request.manifest_hash.trim().is_empty() {
        return error(StatusCode::BAD_REQUEST, "manifest hash is required");
    }
    let Ok(mut connection) = state.db.lock() else {
        return error(StatusCode::INTERNAL_SERVER_ERROR, "database unavailable");
    };
    let generation = connection
        .query_row(
            "SELECT replace_all,status,result_json FROM sync_generations
             WHERE generation_id=?1 AND node_id=?2",
            params![request.generation_id, node_id],
            |row| {
                Ok((
                    row.get::<_, bool>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .optional();
    let Some((replace_all, status, prior_result)) = generation.ok().flatten() else {
        return error(StatusCode::CONFLICT, "generation not found");
    };
    if status == "committed" {
        let Some(prior_result) = prior_result else {
            return error(StatusCode::INTERNAL_SERVER_ERROR, "commit result missing");
        };
        return match serde_json::from_str::<SyncCommitResponse>(&prior_result) {
            Ok(result) => (StatusCode::OK, Json(result)).into_response(),
            Err(_) => error(StatusCode::INTERNAL_SERVER_ERROR, "commit result invalid"),
        };
    }
    if status != "open" {
        return error(StatusCode::CONFLICT, "generation is not open");
    }
    let event_count: usize = connection
        .query_row(
            "SELECT COUNT(*) FROM staged_event_mutations WHERE generation_id=?1",
            [&request.generation_id],
            |row| row.get(0),
        )
        .unwrap_or(usize::MAX);
    let rollup_count: usize = connection
        .query_row(
            "SELECT COUNT(*) FROM staged_rollup_mutations WHERE generation_id=?1",
            [&request.generation_id],
            |row| row.get(0),
        )
        .unwrap_or(usize::MAX);
    if event_count != request.expected_event_mutations
        || rollup_count != request.expected_rollup_mutations
    {
        return error(StatusCode::CONFLICT, "staged mutation count mismatch");
    }

    let event_rows = {
        let mut statement = match connection.prepare(
            "SELECT app_type,request_id,operation,content_hash,payload_json
             FROM staged_event_mutations
             WHERE generation_id=?1 ORDER BY app_type,request_id",
        ) {
            Ok(value) => value,
            Err(_) => {
                return error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "read staged events failed",
                )
            }
        };
        let mapped = statement.query_map([&request.generation_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
            ))
        });
        match mapped {
            Ok(rows) => match rows.collect::<Result<Vec<_>, _>>() {
                Ok(value) => value,
                Err(_) => {
                    return error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "read staged events failed",
                    )
                }
            },
            Err(_) => {
                return error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "read staged events failed",
                )
            }
        }
    };
    let rollup_rows = {
        let mut statement = match connection.prepare(
            "SELECT snapshot_key,operation,content_hash,payload_json
             FROM staged_rollup_mutations WHERE generation_id=?1 ORDER BY snapshot_key",
        ) {
            Ok(value) => value,
            Err(_) => {
                return error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "read staged rollups failed",
                )
            }
        };
        let mapped = statement.query_map([&request.generation_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        });
        match mapped {
            Ok(rows) => match rows.collect::<Result<Vec<_>, _>>() {
                Ok(value) => value,
                Err(_) => {
                    return error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "read staged rollups failed",
                    )
                }
            },
            Err(_) => {
                return error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "read staged rollups failed",
                )
            }
        }
    };
    let providers = {
        let mut statement = match connection.prepare(
            "SELECT app_type,provider_id,name FROM staged_providers
             WHERE generation_id=?1 ORDER BY app_type,provider_id",
        ) {
            Ok(value) => value,
            Err(_) => {
                return error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "read staged providers failed",
                )
            }
        };
        let mapped = statement.query_map([&request.generation_id], |row| {
            Ok(ProviderEntry {
                app_type: row.get(0)?,
                provider_id: row.get(1)?,
                name: row.get(2)?,
            })
        });
        match mapped {
            Ok(rows) => match rows.collect::<Result<Vec<_>, _>>() {
                Ok(value) => value,
                Err(_) => {
                    return error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "read staged providers failed",
                    )
                }
            },
            Err(_) => {
                return error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "read staged providers failed",
                )
            }
        }
    };

    let event_manifest = event_rows
        .iter()
        .map(|(app_type, request_id, operation, content_hash, _)| {
            (
                app_type.as_str(),
                request_id.as_str(),
                operation.as_str(),
                content_hash.as_str(),
            )
        })
        .collect::<Vec<_>>();
    let rollup_manifest = rollup_rows
        .iter()
        .map(|(snapshot_key, operation, content_hash, _)| {
            (
                snapshot_key.as_str(),
                operation.as_str(),
                content_hash.as_str(),
            )
        })
        .collect::<Vec<_>>();
    let actual_manifest = match content_hash(&(event_manifest, rollup_manifest, &providers)) {
        Ok(value) => value,
        Err(_) => return error(StatusCode::INTERNAL_SERVER_ERROR, "hash manifest failed"),
    };
    if actual_manifest != request.manifest_hash {
        return error(StatusCode::CONFLICT, "generation manifest mismatch");
    }

    let transaction = match connection.transaction() {
        Ok(value) => value,
        Err(_) => {
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "start commit transaction failed",
            )
        }
    };
    let mut result = SyncCommitResponse::default();
    if replace_all && usage_cache::mark_node_dirty(&transaction, &node_id).is_err() {
        return error(StatusCode::INTERNAL_SERVER_ERROR, "dirty node cache failed");
    }
    if replace_all
        && transaction
            .execute("DELETE FROM usage_events WHERE node_id=?1", [&node_id])
            .and_then(|_| {
                transaction.execute(
                    "DELETE FROM usage_daily_snapshots WHERE node_id=?1",
                    [&node_id],
                )
            })
            .is_err()
    {
        return error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "clear node snapshot failed",
        );
    }
    for (app_type, request_id, operation, content_hash, payload) in event_rows {
        if operation == "delete" {
            let created_at = transaction
                .query_row(
                    "SELECT created_at FROM usage_events
                     WHERE node_id=?1 AND app_type=?2 AND request_id=?3",
                    params![node_id, app_type, request_id],
                    |row| row.get::<_, i64>(0),
                )
                .optional()
                .ok()
                .flatten();
            if let Some(created_at) = created_at {
                if usage_cache::mark_event_dirty(&transaction, &node_id, created_at).is_err() {
                    return error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "dirty event cache failed",
                    );
                }
            }
            match transaction.execute(
                "DELETE FROM usage_events
                 WHERE node_id=?1 AND app_type=?2 AND request_id=?3",
                params![node_id, app_type, request_id],
            ) {
                Ok(count) => result.deleted += count,
                Err(_) => return error(StatusCode::INTERNAL_SERVER_ERROR, "delete event failed"),
            }
            continue;
        }
        let Some(payload) = payload else {
            return error(StatusCode::BAD_REQUEST, "staged event payload missing");
        };
        let event: UsageEvent = match serde_json::from_str(&payload) {
            Ok(value) => value,
            Err(_) => return error(StatusCode::BAD_REQUEST, "staged event payload invalid"),
        };
        if event.app_type != app_type || event.request_id != request_id {
            return error(StatusCode::BAD_REQUEST, "staged event identity mismatch");
        }
        if upsert_event(&transaction, &node_id, &event, &content_hash, &mut result).is_err() {
            return error(StatusCode::INTERNAL_SERVER_ERROR, "upsert event failed");
        }
    }
    for (snapshot_key, operation, content_hash, payload) in rollup_rows {
        if operation == "delete" {
            let central_key = format!("{node_id}|{snapshot_key}");
            match transaction.execute(
                "DELETE FROM usage_daily_snapshots WHERE snapshot_key=?1",
                [central_key],
            ) {
                Ok(count) => result.deleted += count,
                Err(_) => return error(StatusCode::INTERNAL_SERVER_ERROR, "delete rollup failed"),
            }
            continue;
        }
        let Some(payload) = payload else {
            return error(StatusCode::BAD_REQUEST, "staged rollup payload missing");
        };
        let snapshot: RollupSnapshot = match serde_json::from_str(&payload) {
            Ok(value) => value,
            Err(_) => return error(StatusCode::BAD_REQUEST, "staged rollup payload invalid"),
        };
        if upsert_rollup(&transaction, &node_id, &snapshot, &content_hash).is_err() {
            return error(StatusCode::INTERNAL_SERVER_ERROR, "upsert rollup failed");
        }
        result.rollups += 1;
    }
    let updated_at = chrono::Utc::now().timestamp();
    if transaction
        .execute("DELETE FROM provider_catalog WHERE node_id=?1", [&node_id])
        .is_err()
    {
        return error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "replace provider catalog failed",
        );
    }
    for provider in providers {
        if transaction
            .execute(
                "INSERT INTO provider_catalog (node_id,app_type,provider_id,name,updated_at)
                 VALUES (?1,?2,?3,?4,?5)
                 ON CONFLICT(node_id,app_type,provider_id) DO UPDATE SET
                    name=excluded.name,updated_at=excluded.updated_at",
                params![
                    node_id,
                    provider.app_type,
                    provider.provider_id,
                    provider.name,
                    updated_at
                ],
            )
            .is_err()
        {
            return error(StatusCode::INTERNAL_SERVER_ERROR, "upsert provider failed");
        }
        result.providers += 1;
    }
    let result_json = match serde_json::to_string(&result) {
        Ok(value) => value,
        Err(_) => {
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "encode commit result failed",
            )
        }
    };
    if transaction
        .execute(
            "UPDATE sync_generations SET status='committed',manifest_hash=?1,
             result_json=?2,committed_at=?3 WHERE generation_id=?4",
            params![
                request.manifest_hash,
                result_json,
                chrono::Utc::now().timestamp(),
                request.generation_id
            ],
        )
        .is_err()
    {
        return error(StatusCode::INTERNAL_SERVER_ERROR, "seal generation failed");
    }
    for table in [
        "staged_event_mutations",
        "staged_rollup_mutations",
        "staged_providers",
    ] {
        if transaction
            .execute(
                &format!("DELETE FROM {table} WHERE generation_id=?1"),
                [&request.generation_id],
            )
            .is_err()
        {
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "clear staged generation failed",
            );
        }
    }
    match transaction.commit() {
        Ok(()) => (StatusCode::OK, Json(result)).into_response(),
        Err(_) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "commit generation failed",
        ),
    }
}

pub fn routes() -> Router<ServerState> {
    Router::new()
        .route("/v3/sync/begin", post(begin))
        .route("/v3/sync/events", post(stage_events))
        .route("/v3/sync/rollups", post(stage_rollups))
        .route("/v3/sync/providers", post(stage_providers))
        .route("/v3/sync/commit", post(commit))
        .route("/v2/sync/begin", post(super::upgrade_required))
        .route("/v2/sync/events", post(super::upgrade_required))
        .route("/v2/sync/rollups", post(super::upgrade_required))
        .route("/v2/sync/providers", post(super::upgrade_required))
        .route("/v2/sync/commit", post(super::upgrade_required))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(app_type: &str, provider_id: &str) -> UsageEvent {
        UsageEvent {
            request_id: "shared-request".to_owned(),
            created_at: 100,
            app_type: app_type.to_owned(),
            provider_id: provider_id.to_owned(),
            model: "model".to_owned(),
            request_model: None,
            pricing_model: None,
            input_tokens: 10,
            output_tokens: 2,
            cache_read_tokens: 1,
            cache_creation_tokens: 0,
            input_token_semantics: 2,
            total_cost_usd: "0.1".to_owned(),
            latency_ms: 20,
            status_code: 200,
            is_streaming: true,
            data_source: "proxy".to_owned(),
        }
    }

    #[test]
    fn request_identity_uses_app_not_mutable_provider() {
        let mut connection = crate::init_db(":memory:").unwrap();
        let transaction = connection.transaction().unwrap();
        let mut result = SyncCommitResponse::default();

        upsert_event(
            &transaction,
            "node-a",
            &event("codex", "provider-old"),
            "hash-1",
            &mut result,
        )
        .unwrap();
        upsert_event(
            &transaction,
            "node-a",
            &event("codex", "provider-corrected"),
            "hash-2",
            &mut result,
        )
        .unwrap();
        upsert_event(
            &transaction,
            "node-a",
            &event("claude", "provider-old"),
            "hash-3",
            &mut result,
        )
        .unwrap();
        transaction.commit().unwrap();

        assert_eq!(result.inserted, 2);
        assert_eq!(result.updated, 1);
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM usage_events", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            2
        );
        let corrected: (String, String) = connection
            .query_row(
                "SELECT event_id,provider_id FROM usage_events
                 WHERE node_id='node-a' AND app_type='codex' AND request_id='shared-request'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(corrected.0, event_id("node-a", "codex", "shared-request"));
        assert_eq!(corrected.1, "provider-corrected");
    }
}
