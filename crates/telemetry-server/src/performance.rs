use axum::{
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::time::Instant;

// Runs inside the blocking query task, including JSON serialization. Query time
// includes row decoding/projection; it is deliberately not labelled pure SQL.
pub(crate) fn query_json<T: Serialize>(
    query: impl FnOnce() -> anyhow::Result<T>,
) -> anyhow::Result<Response> {
    conditional_json(query, None)
}

pub(crate) fn conditional_json<T: Serialize>(
    query: impl FnOnce() -> anyhow::Result<T>,
    headers: Option<&HeaderMap>,
) -> anyhow::Result<Response> {
    let start = Instant::now();
    let value = query()?;
    let query_ms = start.elapsed().as_secs_f64() * 1000.0;
    let start = Instant::now();
    let bytes = serde_json::to_vec(&value)?;
    let serialize_ms = start.elapsed().as_secs_f64() * 1000.0;
    let etag = headers.map(|_| format!("\"{:x}\"", Sha256::digest(&bytes)));
    let unchanged = etag.as_ref().is_some_and(|etag| {
        headers
            .and_then(|h| h.get(header::IF_NONE_MATCH))
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.split(',').any(|v| v.trim() == etag || v.trim() == "*"))
    });
    let mut response = if unchanged {
        StatusCode::NOT_MODIFIED.into_response()
    } else {
        bytes.into_response()
    };
    if let Some(etag) = etag {
        response.headers_mut().insert(header::ETAG, etag.parse()?);
        response.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("public, max-age=0, must-revalidate"),
        );
    }
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    response.headers_mut().insert(
        "server-timing",
        format!(
            "query;desc=\"SQL and projection\";dur={query_ms:.3}, serialize;dur={serialize_ms:.3}"
        )
        .parse()?,
    );
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn conditional_json_changes_etag_with_content_and_reports_timing() {
        let first = conditional_json(
            || Ok(serde_json::json!({"providers":[]})),
            Some(&HeaderMap::new()),
        )
        .unwrap();
        let etag = first.headers()[header::ETAG].clone();
        assert!(first.headers().contains_key("server-timing"));
        let mut headers = HeaderMap::new();
        headers.insert(header::IF_NONE_MATCH, etag.clone());
        let same =
            conditional_json(|| Ok(serde_json::json!({"providers":[]})), Some(&headers)).unwrap();
        assert_eq!(same.status(), StatusCode::NOT_MODIFIED);
        let changed =
            conditional_json(|| Ok(serde_json::json!({"providers":[1]})), Some(&headers)).unwrap();
        assert_eq!(changed.status(), StatusCode::OK);
        assert_ne!(changed.headers()[header::ETAG], etag);
    }
}
