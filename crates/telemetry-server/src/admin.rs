use crate::{nodes, ServerState};
use axum::{
    extract::{Path, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::{get, patch, post},
    Json, Router,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::time::{Duration, Instant};
use uuid::Uuid;

const SESSION_COOKIE: &str = "telemetry_admin_session";
const SESSION_TTL: Duration = Duration::from_secs(12 * 60 * 60);
const ADMIN_HTML: &str = include_str!("../web/admin.html");
const ADMIN_CSS: &str = include_str!("../web/admin.css");
const ADMIN_JS: &str = include_str!("../web/admin.js");

#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    pub password: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeNameRequest {
    pub node_name: String,
}

fn json_error(status: StatusCode, code: &str, message: impl Into<String>) -> Response {
    (
        status,
        Json(serde_json::json!({
            "code": code,
            "message": message.into(),
        })),
    )
        .into_response()
}

fn cookie_value(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
        .and_then(|cookies| {
            cookies.split(';').find_map(|cookie| {
                let (name, value) = cookie.trim().split_once('=')?;
                (name == SESSION_COOKIE).then(|| value.to_owned())
            })
        })
}

fn valid_password(expected: &str, supplied: &str) -> bool {
    let expected = Sha256::digest(expected.as_bytes());
    let supplied = Sha256::digest(supplied.as_bytes());
    expected
        .iter()
        .zip(supplied.iter())
        .fold(0u8, |difference, (left, right)| difference | (left ^ right))
        == 0
}

fn authenticated(state: &ServerState, headers: &HeaderMap) -> bool {
    if state.admin_password.is_none() {
        return false;
    }
    let Some(session) = cookie_value(headers) else {
        return false;
    };
    let Ok(mut sessions) = state.admin_sessions.lock() else {
        return false;
    };
    let Some(expires_at) = sessions.get(&session).copied() else {
        return false;
    };
    if expires_at <= Instant::now() {
        sessions.remove(&session);
        return false;
    }
    true
}

fn require_admin(state: &ServerState, headers: &HeaderMap) -> Result<(), Box<Response>> {
    if state.admin_password.is_none() {
        return Err(Box::new(json_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "admin_disabled",
            "ADMIN_PASSWORD is not configured",
        )));
    }
    if authenticated(state, headers) {
        Ok(())
    } else {
        Err(Box::new(json_error(
            StatusCode::UNAUTHORIZED,
            "admin_auth_required",
            "administrator login required",
        )))
    }
}

fn normalize_node_name(name: String) -> Result<String, Box<Response>> {
    let name = name.trim().to_owned();
    if name.is_empty() {
        return Err(Box::new(json_error(
            StatusCode::BAD_REQUEST,
            "invalid_node_name",
            "nodeName cannot be empty",
        )));
    }
    if name.len() > 128 {
        return Err(Box::new(json_error(
            StatusCode::BAD_REQUEST,
            "invalid_node_name",
            "nodeName cannot exceed 128 bytes",
        )));
    }
    Ok(name)
}

fn database_error(error: impl std::fmt::Display) -> Response {
    json_error(
        StatusCode::SERVICE_UNAVAILABLE,
        "database_unavailable",
        error.to_string(),
    )
}

async fn index() -> Html<&'static str> {
    Html(ADMIN_HTML)
}

async fn styles() -> Response {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        ADMIN_CSS,
    )
        .into_response()
}

async fn script() -> Response {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        ADMIN_JS,
    )
        .into_response()
}

async fn login(State(state): State<ServerState>, Json(payload): Json<LoginRequest>) -> Response {
    let Some(expected) = state.admin_password.as_deref() else {
        return json_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "admin_disabled",
            "ADMIN_PASSWORD is not configured",
        );
    };
    if !valid_password(expected, &payload.password) {
        return json_error(
            StatusCode::UNAUTHORIZED,
            "invalid_password",
            "invalid administrator password",
        );
    }
    let session = format!("as_{}", Uuid::new_v4().simple());
    if let Ok(mut sessions) = state.admin_sessions.lock() {
        sessions.insert(session.clone(), Instant::now() + SESSION_TTL);
    } else {
        return json_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "session_unavailable",
            "administrator session store is unavailable",
        );
    }
    let cookie = format!(
        "{SESSION_COOKIE}={session}; Path=/admin; Max-Age={}; HttpOnly; SameSite=Lax",
        SESSION_TTL.as_secs()
    );
    let mut response = Json(serde_json::json!({"ok": true})).into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&cookie).expect("session cookie is valid"),
    );
    response
}

async fn logout(State(state): State<ServerState>, headers: HeaderMap) -> Response {
    if let Some(session) = cookie_value(&headers) {
        if let Ok(mut sessions) = state.admin_sessions.lock() {
            sessions.remove(&session);
        }
    }
    let mut response = Json(serde_json::json!({"ok": true})).into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_static(
            "telemetry_admin_session=; Path=/admin; Max-Age=0; HttpOnly; SameSite=Lax",
        ),
    );
    response
}

async fn session(State(state): State<ServerState>, headers: HeaderMap) -> Response {
    if state.admin_password.is_none() {
        return json_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "admin_disabled",
            "ADMIN_PASSWORD is not configured",
        );
    }
    if authenticated(&state, &headers) {
        Json(serde_json::json!({"authenticated": true, "adminEnabled": true})).into_response()
    } else {
        json_error(
            StatusCode::UNAUTHORIZED,
            "admin_auth_required",
            "administrator login required",
        )
    }
}

async fn list_nodes(State(state): State<ServerState>, headers: HeaderMap) -> Response {
    if let Err(response) = require_admin(&state, &headers) {
        return *response;
    }
    let Ok(db) = state.db.lock() else {
        return database_error("database lock unavailable");
    };
    match nodes::list(&db) {
        Ok(nodes) => Json(nodes).into_response(),
        Err(error) => database_error(error),
    }
}

async fn create_node(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Json(payload): Json<NodeNameRequest>,
) -> Response {
    if let Err(response) = require_admin(&state, &headers) {
        return *response;
    }
    let node_name = match normalize_node_name(payload.node_name) {
        Ok(value) => value,
        Err(response) => return *response,
    };
    let Ok(db) = state.db.lock() else {
        return database_error("database lock unavailable");
    };
    match nodes::create(&db, &node_name) {
        Ok((node, token)) => Json(serde_json::json!({
            "node": node,
            "token": token,
            "tokenShownOnce": true,
        }))
        .into_response(),
        Err(error) => json_error(
            StatusCode::CONFLICT,
            "node_create_failed",
            error.to_string(),
        ),
    }
}

async fn rename_node(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Path(uuid): Path<String>,
    Json(payload): Json<NodeNameRequest>,
) -> Response {
    if let Err(response) = require_admin(&state, &headers) {
        return *response;
    }
    let node_name = match normalize_node_name(payload.node_name) {
        Ok(value) => value,
        Err(response) => return *response,
    };
    let Ok(db) = state.db.lock() else {
        return database_error("database lock unavailable");
    };
    match nodes::rename(&db, &uuid, &node_name) {
        Ok(Some(node)) => Json(node).into_response(),
        Ok(None) => json_error(StatusCode::NOT_FOUND, "node_not_found", "node not found"),
        Err(error) => json_error(
            StatusCode::CONFLICT,
            "node_rename_failed",
            error.to_string(),
        ),
    }
}

async fn regenerate_token(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Path(uuid): Path<String>,
) -> Response {
    if let Err(response) = require_admin(&state, &headers) {
        return *response;
    }
    let Ok(db) = state.db.lock() else {
        return database_error("database lock unavailable");
    };
    match nodes::regenerate_token(&db, &uuid) {
        Ok(Some((node, token))) => Json(serde_json::json!({
            "node": node,
            "token": token,
            "tokenShownOnce": true,
        }))
        .into_response(),
        Ok(None) => json_error(StatusCode::NOT_FOUND, "node_not_found", "node not found"),
        Err(error) => database_error(error),
    }
}

async fn revoke_token(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Path(uuid): Path<String>,
) -> Response {
    if let Err(response) = require_admin(&state, &headers) {
        return *response;
    }
    let Ok(db) = state.db.lock() else {
        return database_error("database lock unavailable");
    };
    match nodes::revoke_token(&db, &uuid) {
        Ok(Some(node)) => Json(node).into_response(),
        Ok(None) => json_error(StatusCode::NOT_FOUND, "node_not_found", "node not found"),
        Err(error) => database_error(error),
    }
}

pub fn routes() -> Router<ServerState> {
    Router::new()
        .route("/admin", get(index))
        .route("/admin/", get(index))
        .route("/admin/styles.css", get(styles))
        .route("/admin/app.js", get(script))
        .route("/admin/login", post(login))
        .route("/admin/logout", post(logout))
        .route("/admin/api/session", get(session))
        .route("/admin/api/nodes", get(list_nodes).post(create_node))
        .route("/admin/api/nodes/:uuid", patch(rename_node))
        .route(
            "/admin/api/nodes/:uuid/token",
            post(regenerate_token).delete(revoke_token),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{init_db, nodes, router};
    use axum::{body::Body, http::Request};
    use std::path::PathBuf;
    use tower::ServiceExt;

    async fn response_json(response: Response) -> serde_json::Value {
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    fn json_body(value: serde_json::Value) -> Body {
        Body::from(serde_json::to_vec(&value).unwrap())
    }

    #[tokio::test]
    async fn admin_login_and_node_token_lifecycle() {
        let state = ServerState::new(
            init_db(":memory:").unwrap(),
            PathBuf::from("telemetry.db"),
            Some("unit-test-admin".to_owned()),
        );
        let app = router(state.clone());

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/admin/login")
                    .header("content-type", "application/json")
                    .body(json_body(serde_json::json!({"password":"wrong"})))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/admin/login")
                    .header("content-type", "application/json")
                    .body(json_body(serde_json::json!({"password":"unit-test-admin"})))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let cookie = response
            .headers()
            .get(header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/admin/api/nodes")
                    .header(header::COOKIE, &cookie)
                    .header("content-type", "application/json")
                    .body(json_body(serde_json::json!({"nodeName":"test-node"})))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let created = response_json(response).await;
        let uuid = created["node"]["uuid"].as_str().unwrap().to_owned();
        let token = created["token"].as_str().unwrap().to_owned();
        assert!(created["tokenShownOnce"].as_bool().unwrap());
        assert!(nodes::authorized_node(
            &state.db.lock().unwrap(),
            &uuid,
            &token
        ));

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri(format!("/admin/api/nodes/{uuid}"))
                    .header(header::COOKIE, &cookie)
                    .header("content-type", "application/json")
                    .body(json_body(serde_json::json!({"nodeName":"renamed-node"})))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/admin/api/nodes/{uuid}/token"))
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let regenerated = response_json(response).await;
        let replacement = regenerated["token"].as_str().unwrap();
        assert!(!nodes::authorized_node(
            &state.db.lock().unwrap(),
            &uuid,
            &token
        ));
        assert!(nodes::authorized_node(
            &state.db.lock().unwrap(),
            &uuid,
            replacement
        ));

        let response = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/admin/api/nodes/{uuid}/token"))
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(!nodes::authorized_node(
            &state.db.lock().unwrap(),
            &uuid,
            replacement
        ));
    }
}
