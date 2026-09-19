use crate::{admin, ServerState};
use axum::{
    extract::State,
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Settings {
    pub version: u32,
    pub quota_defaults: QuotaDefaults,
    pub quota_provider_aliases: Vec<ProviderAlias>,
    #[serde(default)]
    pub dashboard_defaults: DashboardDefaults,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DashboardDefaults {
    #[serde(default = "default_dashboard_range_preset")]
    pub range_preset: String,
    #[serde(default = "default_dashboard_time_format")]
    pub time_format: String,
    #[serde(default)]
    pub model_billing_multipliers: Vec<ModelBillingMultiplier>,
    #[serde(default)]
    pub last_reset: Option<DashboardLastReset>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelBillingMultiplier {
    pub model: String,
    pub multiplier: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DashboardLastReset {
    pub node_id: String,
    pub provider_id: String,
    pub metric_key: String,
    pub metric_kind: String,
    #[serde(default)]
    pub unit: Option<String>,
}

fn default_dashboard_range_preset() -> String {
    "24h".into()
}

fn default_dashboard_time_format() -> String {
    "24h".into()
}

impl Default for DashboardDefaults {
    fn default() -> Self {
        Self {
            range_preset: default_dashboard_range_preset(),
            time_format: default_dashboard_time_format(),
            model_billing_multipliers: Vec::new(),
            last_reset: None,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QuotaDefaults {
    pub providers: Option<Vec<ProviderSelection>>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderSelection {
    pub node_id: String,
    pub provider_id: String,
    pub metrics: Option<Vec<MetricSelection>>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MetricSelection {
    pub key: String,
    pub kind: String,
    pub unit: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderAlias {
    pub node_id: String,
    pub provider_id: String,
    pub alias: String,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            version: 1,
            quota_defaults: QuotaDefaults::default(),
            quota_provider_aliases: Vec::new(),
            dashboard_defaults: DashboardDefaults::default(),
        }
    }
}
fn valid_text(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
}
fn normalize(mut settings: Settings) -> anyhow::Result<Settings> {
    anyhow::ensure!(settings.version == 1, "unsupported settings version");
    anyhow::ensure!(
        matches!(
            settings.dashboard_defaults.range_preset.as_str(),
            "today" | "1h" | "24h" | "7d" | "14d" | "30d" | "1y" | "last-reset" | "all"
        ),
        "invalid dashboard range preset"
    );
    anyhow::ensure!(
        matches!(
            settings.dashboard_defaults.time_format.as_str(),
            "12h" | "24h"
        ),
        "invalid dashboard time format"
    );
    anyhow::ensure!(
        settings.dashboard_defaults.model_billing_multipliers.len() <= 1000,
        "too many model billing multipliers"
    );
    let mut models = BTreeSet::new();
    for entry in &mut settings.dashboard_defaults.model_billing_multipliers {
        entry.model = entry.model.trim().to_owned();
        anyhow::ensure!(
            valid_text(&entry.model)
                && entry.multiplier.is_finite()
                && (0.0..=1000.0).contains(&entry.multiplier),
            "invalid model billing multiplier"
        );
        anyhow::ensure!(
            models.insert(entry.model.clone()),
            "duplicate model billing multiplier"
        );
    }
    if let Some(last_reset) = &mut settings.dashboard_defaults.last_reset {
        anyhow::ensure!(
            valid_text(&last_reset.node_id)
                && valid_text(&last_reset.provider_id)
                && valid_text(&last_reset.metric_key)
                && matches!(
                    last_reset.metric_kind.as_str(),
                    "balance" | "utilizationPercent"
                )
                && last_reset
                    .unit
                    .as_deref()
                    .is_none_or(|unit| unit.is_empty() || valid_text(unit)),
            "invalid dashboard reset metric"
        );
        if let Some(unit) = &mut last_reset.unit {
            *unit = unit.trim().to_owned();
        }
    }
    if let Some(providers) = &settings.quota_defaults.providers {
        anyhow::ensure!(providers.len() <= 1000, "too many providers");
        let mut identities = BTreeSet::new();
        for provider in providers {
            anyhow::ensure!(
                valid_text(&provider.node_id) && valid_text(&provider.provider_id),
                "invalid provider identity"
            );
            anyhow::ensure!(
                identities.insert((&provider.node_id, &provider.provider_id)),
                "duplicate provider"
            );
            if let Some(metrics) = &provider.metrics {
                anyhow::ensure!(metrics.len() <= 1000, "too many metrics");
                let mut identities = BTreeSet::new();
                for metric in metrics {
                    anyhow::ensure!(
                        valid_text(&metric.key)
                            && matches!(metric.kind.as_str(), "balance" | "utilizationPercent")
                            && metric
                                .unit
                                .as_deref()
                                .is_none_or(|unit| unit.is_empty() || valid_text(unit)),
                        "invalid metric identity"
                    );
                    anyhow::ensure!(
                        identities.insert((
                            &metric.key,
                            &metric.kind,
                            metric.unit.as_deref().unwrap_or("")
                        )),
                        "duplicate metric"
                    );
                }
            }
        }
    }
    anyhow::ensure!(
        settings.quota_provider_aliases.len() <= 1000,
        "too many aliases"
    );
    let mut identities = BTreeSet::new();
    for entry in &mut settings.quota_provider_aliases {
        anyhow::ensure!(
            valid_text(&entry.node_id) && valid_text(&entry.provider_id),
            "invalid alias identity"
        );
        anyhow::ensure!(
            identities.insert((entry.node_id.clone(), entry.provider_id.clone())),
            "duplicate alias"
        );
        entry.alias = entry.alias.trim().to_owned();
        anyhow::ensure!(
            entry.alias.is_empty() || valid_text(&entry.alias),
            "alias must be at most 256 bytes and contain no control characters"
        );
    }
    settings
        .quota_provider_aliases
        .retain(|entry| !entry.alias.is_empty());
    Ok(settings)
}
fn persist(path: &Path, settings: &Settings) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(".settings-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| -> anyhow::Result<()> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(&serde_json::to_vec_pretty(settings)?)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}
fn load(path: &Path) -> anyhow::Result<Settings> {
    match fs::read(path) {
        Ok(bytes) => normalize(serde_json::from_slice(&bytes)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let settings = Settings::default();
            persist(path, &settings)?;
            Ok(settings)
        }
        Err(error) => Err(error.into()),
    }
}
pub(crate) fn read(state: &ServerState) -> anyhow::Result<Settings> {
    let mut cached = state
        .settings
        .lock()
        .map_err(|_| anyhow::anyhow!("settings lock unavailable"))?;
    if cached.is_none() {
        *cached = Some(load(&state.db_path.with_file_name("settings.json"))?);
    }
    Ok(cached.as_ref().expect("settings initialized").clone())
}
fn save(state: &ServerState, settings: Settings) -> anyhow::Result<Settings> {
    let mut cached = state
        .settings
        .lock()
        .map_err(|_| anyhow::anyhow!("settings lock unavailable"))?;
    let path = state.db_path.with_file_name("settings.json");
    // Refuse to overwrite a malformed existing file, including on a first PUT.
    if cached.is_none() {
        *cached = Some(load(&path)?);
    }
    persist(&path, &settings)?;
    *cached = Some(settings.clone());
    Ok(settings)
}
fn failure(status: StatusCode, error: impl std::fmt::Display) -> Response {
    (
        status,
        Json(serde_json::json!({"code": "settings_error", "message": error.to_string()})),
    )
        .into_response()
}
pub(crate) async fn public_get(State(state): State<ServerState>) -> Response {
    match read(&state) {
        Ok(settings) => Json(settings).into_response(),
        Err(error) => failure(StatusCode::SERVICE_UNAVAILABLE, error),
    }
}
pub(crate) async fn admin_get(State(state): State<ServerState>, headers: HeaderMap) -> Response {
    if let Err(response) = admin::require_admin(&state, &headers) {
        return *response;
    }
    let result = (|| -> anyhow::Result<serde_json::Value> {
        let settings = read(&state)?;
        let db = state
            .db
            .lock()
            .map_err(|_| anyhow::anyhow!("database lock unavailable"))?;
        let mut statement = db.prepare(
            "SELECT s.node_id,n.node_name,s.provider_id,s.provider_name
            FROM quota_provider_states s JOIN nodes n ON n.uuid=s.node_id
            WHERE s.app_type='codex' ORDER BY n.node_name,s.provider_name",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;
        let mut providers = Vec::new();
        for row in rows {
            let (node_id, node_name, provider_id, provider_name) = row?;
            let mut metrics = db.prepare("WITH ranked AS (
                SELECT m.metric_key,m.metric_label,m.metric_kind,m.unit,
                    ROW_NUMBER() OVER (PARTITION BY m.metric_key,m.metric_kind,COALESCE(m.unit,'')
                    ORDER BY o.sampled_at DESC,o.observation_id DESC) AS position
                FROM quota_metrics m JOIN quota_observations o
                  ON m.node_id=o.node_id AND m.observation_id=o.observation_id
                WHERE o.node_id=?1 AND o.provider_id=?2 AND o.app_type='codex'
            ) SELECT metric_key,metric_label,metric_kind,unit FROM ranked WHERE position=1 ORDER BY metric_key")?;
            let entries = metrics
                .query_map([&node_id, &provider_id], |row| {
                    Ok(serde_json::json!({
                        "key": row.get::<_, String>(0)?, "label": row.get::<_, String>(1)?,
                        "kind": row.get::<_, String>(2)?, "unit": row.get::<_, Option<String>>(3)?
                    }))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            providers.push(serde_json::json!({"nodeId": node_id, "nodeName": node_name,
                "providerId": provider_id, "providerName": provider_name, "metrics": entries}));
        }
        Ok(serde_json::json!({"settings": settings, "providers": providers}))
    })();
    match result {
        Ok(result) => Json(result).into_response(),
        Err(error) => failure(StatusCode::SERVICE_UNAVAILABLE, error),
    }
}
pub(crate) async fn admin_put(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Json(settings): Json<Settings>,
) -> Response {
    if let Err(response) = admin::require_admin(&state, &headers) {
        return *response;
    }
    let settings = match normalize(settings) {
        Ok(settings) => settings,
        Err(error) => return failure(StatusCode::BAD_REQUEST, error),
    };
    match save(&state, settings) {
        Ok(settings) => Json(settings).into_response(),
        Err(error) => failure(StatusCode::SERVICE_UNAVAILABLE, error),
    }
}
pub(crate) async fn script() -> Response {
    (
        [
            (header::CONTENT_TYPE, "text/javascript; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        include_str!("../web/quota-settings.js"),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{to_bytes, Body},
        extract::connect_info::MockConnectInfo,
        http::Request,
    };
    use tower::ServiceExt;

    #[test]
    fn persistence_validation_and_corrupt_files() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        assert!(load(&path).unwrap().quota_defaults.providers.is_none());
        let legacy_path = directory.path().join("legacy-settings.json");
        fs::write(
            &legacy_path,
            br#"{"version":1,"quotaDefaults":{},"quotaProviderAliases":[]}"#,
        )
        .unwrap();
        let legacy = load(&legacy_path).unwrap();
        assert_eq!(legacy.dashboard_defaults.range_preset, "24h");
        assert_eq!(legacy.dashboard_defaults.time_format, "24h");
        assert!(legacy
            .dashboard_defaults
            .model_billing_multipliers
            .is_empty());
        let mut settings = Settings::default();
        settings.quota_provider_aliases = vec![
            ProviderAlias {
                node_id: "a".into(),
                provider_id: "same".into(),
                alias: " Alpha ".into(),
            },
            ProviderAlias {
                node_id: "b".into(),
                provider_id: "same".into(),
                alias: "Beta".into(),
            },
        ];
        settings.quota_defaults.providers = Some(vec![ProviderSelection {
            node_id: "a".into(),
            provider_id: "same".into(),
            metrics: Some(vec![MetricSelection {
                key: "weekly".into(),
                kind: "utilizationPercent".into(),
                unit: Some("%".into()),
            }]),
        }]);
        settings.dashboard_defaults.range_preset = "last-reset".into();
        settings.dashboard_defaults.time_format = "12h".into();
        settings.dashboard_defaults.model_billing_multipliers = vec![
            ModelBillingMultiplier {
                model: " gpt-5 ".into(),
                multiplier: 1.25,
            },
            ModelBillingMultiplier {
                model: "claude-sonnet".into(),
                multiplier: 0.8,
            },
        ];
        settings.dashboard_defaults.last_reset = Some(DashboardLastReset {
            node_id: "a".into(),
            provider_id: "same".into(),
            metric_key: "five-hour".into(),
            metric_kind: "utilizationPercent".into(),
            unit: Some(" % ".into()),
        });
        let settings = normalize(settings).unwrap();
        persist(&path, &settings).unwrap();
        let reloaded = load(&path).unwrap();
        assert_eq!(reloaded.quota_provider_aliases[0].alias, "Alpha");
        assert_eq!(reloaded.quota_provider_aliases[1].alias, "Beta");
        assert_eq!(
            reloaded.quota_defaults.providers.unwrap()[0]
                .metrics
                .as_ref()
                .unwrap()[0]
                .key,
            "weekly"
        );
        assert_eq!(reloaded.dashboard_defaults.range_preset, "last-reset");
        assert_eq!(reloaded.dashboard_defaults.time_format, "12h");
        assert_eq!(
            reloaded.dashboard_defaults.model_billing_multipliers,
            vec![
                ModelBillingMultiplier {
                    model: "gpt-5".into(),
                    multiplier: 1.25,
                },
                ModelBillingMultiplier {
                    model: "claude-sonnet".into(),
                    multiplier: 0.8,
                },
            ]
        );
        assert_eq!(
            reloaded
                .dashboard_defaults
                .last_reset
                .unwrap()
                .unit
                .as_deref(),
            Some("%")
        );
        let mut invalid = Settings::default();
        invalid.dashboard_defaults.range_preset = "custom".into();
        assert!(normalize(invalid).is_err());
        let mut invalid = Settings::default();
        invalid.dashboard_defaults.time_format = "18h".into();
        assert!(normalize(invalid).is_err());
        let mut invalid = Settings::default();
        invalid.dashboard_defaults.model_billing_multipliers = vec![ModelBillingMultiplier {
            model: "gpt-5".into(),
            multiplier: -0.01,
        }];
        assert!(normalize(invalid).is_err());
        let mut invalid = Settings::default();
        invalid.dashboard_defaults.model_billing_multipliers = vec![ModelBillingMultiplier {
            model: "gpt-5".into(),
            multiplier: 1000.01,
        }];
        assert!(normalize(invalid).is_err());
        let mut invalid = Settings::default();
        invalid.dashboard_defaults.model_billing_multipliers = vec![ModelBillingMultiplier {
            model: "gpt-5".into(),
            multiplier: f64::NAN,
        }];
        assert!(normalize(invalid).is_err());
        let mut invalid = Settings::default();
        invalid.dashboard_defaults.model_billing_multipliers = vec![
            ModelBillingMultiplier {
                model: "gpt-5".into(),
                multiplier: 1.0,
            },
            ModelBillingMultiplier {
                model: " gpt-5 ".into(),
                multiplier: 2.0,
            },
        ];
        assert!(normalize(invalid).is_err());
        let mut invalid = Settings::default();
        invalid.dashboard_defaults.model_billing_multipliers = vec![ModelBillingMultiplier {
            model: "  ".into(),
            multiplier: 1.0,
        }];
        assert!(normalize(invalid).is_err());
        let mut invalid = Settings::default();
        invalid.dashboard_defaults.last_reset = Some(DashboardLastReset {
            node_id: "node".into(),
            provider_id: "provider".into(),
            metric_key: "metric".into(),
            metric_kind: "unknown".into(),
            unit: None,
        });
        assert!(normalize(invalid).is_err());
        let blocked = directory.path().join("directory.json");
        fs::create_dir(&blocked).unwrap();
        assert!(persist(&blocked, &settings).is_err());
        assert_eq!(load(&path).unwrap().quota_provider_aliases.len(), 2);
        fs::write(&path, b"broken").unwrap();
        assert!(load(&path).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"broken");
    }

    #[tokio::test]
    async fn settings_routes_require_admin_and_public_route_is_local_only() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("telemetry.db");
        let state = ServerState::new(
            crate::init_db(&path).unwrap(),
            path,
            Some("test-password".into()),
        );
        let app = crate::router(state.clone());
        for method in ["GET", "PUT"] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri("/admin/api/settings")
                        .header("content-type", "application/json")
                        .body(Body::from(
                            serde_json::to_vec(&Settings::default()).unwrap(),
                        ))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        }
        let login = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/admin/login")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"password":"test-password"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        let cookie = login.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let mut settings = Settings::default();
        settings.quota_defaults.providers = Some(vec![]);
        settings.dashboard_defaults.model_billing_multipliers = vec![ModelBillingMultiplier {
            model: "gpt-5".into(),
            multiplier: 1.25,
        }];
        let saved = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/admin/api/settings")
                    .header("cookie", &cookie)
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&settings).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(saved.status(), StatusCode::OK);
        let get = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/admin/api/settings")
                    .header("cookie", &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(get.status(), StatusCode::OK);
        let bytes = to_bytes(get.into_body(), usize::MAX).await.unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()["settings"]
                ["quotaDefaults"]["providers"],
            serde_json::json!([])
        );
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()["settings"]
                ["dashboardDefaults"]["rangePreset"],
            "24h"
        );
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()["settings"]
                ["dashboardDefaults"]["modelBillingMultipliers"],
            serde_json::json!([{"model": "gpt-5", "multiplier": 1.25}])
        );
        for (peer, expected) in [
            ("127.0.0.1:1234", StatusCode::OK),
            ("192.0.2.1:1234", StatusCode::FORBIDDEN),
        ] {
            let response = app
                .clone()
                .layer(MockConnectInfo(
                    peer.parse::<std::net::SocketAddr>().unwrap(),
                ))
                .oneshot(
                    Request::builder()
                        .uri("/v3/dashboard/settings")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), expected);
        }
        let previous = read(&state).unwrap();
        let settings_path = state.db_path.with_file_name("settings.json");
        fs::remove_file(&settings_path).unwrap();
        fs::create_dir(&settings_path).unwrap();
        assert!(save(&state, Settings::default()).is_err());
        assert_eq!(
            read(&state)
                .unwrap()
                .quota_defaults
                .providers
                .unwrap()
                .len(),
            previous.quota_defaults.providers.unwrap().len()
        );
    }
}
