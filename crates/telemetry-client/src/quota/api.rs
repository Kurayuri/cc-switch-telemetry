//! Authenticated cc-switch loopback transport. No CLI, upstream credentials or
//! source SQLite reads; normalized history/upload remain in the parent module.
use super::*;
use std::{fs::OpenOptions, io::Read};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Descriptor {
    schema_version: u8,
    base_url: String,
    token: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Catalog {
    schema_version: u8,
    app: String,
    providers: Vec<CodexProvider>,
}
#[derive(Clone, Copy)]
pub(super) enum ApiError {
    Unavailable,
    Invalid,
    TimedOut,
}
impl From<reqwest::Error> for ApiError {
    fn from(value: reqwest::Error) -> Self {
        if value.is_timeout() {
            Self::TimedOut
        } else {
            Self::Unavailable
        }
    }
}
pub(super) struct QuotaApi {
    client: reqwest::Client,
    descriptor: Descriptor,
}
impl QuotaApi {
    pub fn connect(path: &Path, timeout: Duration) -> Result<Self, ApiError> {
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let file = options.open(path).map_err(|_| ApiError::Unavailable)?;
        let meta = file.metadata().map_err(|_| ApiError::Unavailable)?;
        if !meta.is_file() || meta.len() > 4096 {
            return Err(ApiError::Invalid);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if meta.mode() & 0o077 != 0
                || meta.nlink() != 1
                || meta.uid() != unsafe { libc::geteuid() }
            {
                return Err(ApiError::Invalid);
            }
        }
        let mut data = Vec::new();
        file.take(4097)
            .read_to_end(&mut data)
            .map_err(|_| ApiError::Unavailable)?;
        if data.len() > 4096 {
            return Err(ApiError::Invalid);
        }
        let descriptor: Descriptor =
            serde_json::from_slice(&data).map_err(|_| ApiError::Invalid)?;
        validate(&descriptor)?;
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(timeout)
            .build()
            .map_err(ApiError::from)?;
        Ok(Self { client, descriptor })
    }
    async fn json<T: DeserializeOwned>(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<T, ApiError> {
        let mut response = request.bearer_auth(&self.descriptor.token).send().await?;
        if !response.status().is_success() {
            return Err(ApiError::Unavailable);
        }
        if response
            .content_length()
            .is_some_and(|size| size > MAX_API_RESPONSE_BYTES as u64)
        {
            return Err(ApiError::Invalid);
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            if bytes.len() + chunk.len() > MAX_API_RESPONSE_BYTES {
                return Err(ApiError::Invalid);
            }
            bytes.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&bytes).map_err(|_| ApiError::Invalid)
    }
    pub async fn providers(&self) -> Result<Vec<CodexProvider>, ApiError> {
        let catalog: Catalog = self
            .json(
                self.client
                    .get(format!("{}/v1/quota/providers", self.descriptor.base_url)),
            )
            .await?;
        if catalog.schema_version != 1 || catalog.app != "codex" || catalog.providers.len() > 1024 {
            return Err(ApiError::Invalid);
        }
        let mut ids = std::collections::HashSet::new();
        if catalog.providers.iter().any(|p| {
            p.id.is_empty()
                || p.id.len() > 512
                || p.id.chars().any(char::is_control)
                || !ids.insert(&p.id)
        }) {
            return Err(ApiError::Invalid);
        }
        Ok(catalog.providers)
    }
    pub async fn query(&self, provider: &CodexProvider) -> CollectedQuota {
        let request = self
            .client
            .post(format!("{}/v1/quota/query", self.descriptor.base_url))
            .json(&serde_json::json!({ "app": "codex", "providerId": provider.id }));
        let result: Result<serde_json::Value, _> = self.json(request).await;
        match result {
            Ok(value) if value.get("schemaVersion").and_then(|v| v.as_u64()) == Some(1) => {
                match serde_json::from_value::<ApiQuotaOutput>(value) {
                    Ok(output) => normalize_output(provider, output),
                    Err(_) => failed(provider, ApiError::Invalid),
                }
            }
            Ok(_) => failed(provider, ApiError::Invalid),
            Err(error) => failed(provider, error),
        }
    }
}
fn validate(d: &Descriptor) -> Result<(), ApiError> {
    let url = reqwest::Url::parse(&d.base_url).map_err(|_| ApiError::Invalid)?;
    if d.schema_version != 1
        || url.scheme() != "http"
        || url.host_str() != Some("127.0.0.1")
        || url.port().is_none_or(|p| p == 0)
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
        || d.token.len() != 64
        || !d.token.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(ApiError::Invalid);
    }
    Ok(())
}
pub(super) fn failed(provider: &CodexProvider, error: ApiError) -> CollectedQuota {
    let status = match error {
        ApiError::Unavailable => QuotaProviderStatus::QueryFailed,
        ApiError::Invalid => QuotaProviderStatus::InvalidOutput,
        ApiError::TimedOut => QuotaProviderStatus::TimedOut,
    };
    let mut result = state_only(provider, status, None, chrono::Utc::now().timestamp());
    result.state.diagnostic_code = Some("quota_api_unavailable".into());
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        http::{HeaderMap, StatusCode},
        routing::{get, post},
        Json, Router,
    };
    use serde_json::json;
    fn descriptor(path: &Path, url: &str) {
        std::fs::write(
            path,
            json!({"schemaVersion":1, "baseUrl":url,"token":"a".repeat(64)}).to_string(),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
    }
    async fn mock(
        value: serde_json::Value,
        status: StatusCode,
        delay: Duration,
    ) -> (String, tokio::task::JoinHandle<()>) {
        let router = Router::new().route("/v1/quota/providers", get(|| async { Json(json!({"schemaVersion":1,"app":"codex","providers":[{"id":"provider-a","name":"A"}]})) }))
            .route("/v1/quota/query", post(move |headers: HeaderMap, Json(input): Json<serde_json::Value>| {
                let value = value.clone(); async move {
                    assert_eq!(headers["authorization"], format!("Bearer {}", "a".repeat(64)));
                    assert_eq!(input, json!({"app":"codex","providerId":"provider-a"}));
                    tokio::time::sleep(delay).await;
                    (status, Json(value))
                }
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        (url, task)
    }
    fn success() -> serde_json::Value {
        let mut value: serde_json::Value =
            serde_json::from_str(&super::super::tests::subscription_json("ok", true)).unwrap();
        value["schemaVersion"] = json!(1);
        value
    }
    #[test]
    fn discovery_rejects_nonlocal_urls_versions_and_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("quota-api.json");
        for url in [
            "http://example.com:1234",
            "http://127.0.0.1.evil:1234",
            "https://127.0.0.1:1234",
            "http://127.0.0.1:1234/path",
            "http://user@127.0.0.1:1234",
            "http://127.0.0.1:1234?token=bad",
        ] {
            descriptor(&path, url);
            assert!(QuotaApi::connect(&path, Duration::from_secs(1)).is_err());
        }
        descriptor(&path, "http://127.0.0.1:1234");
        assert!(QuotaApi::connect(&path, Duration::from_secs(1)).is_ok());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert!(QuotaApi::connect(&path, Duration::from_secs(1)).is_err());
            let link = dir.path().join("link");
            std::os::unix::fs::symlink(&path, &link).unwrap();
            assert!(QuotaApi::connect(&link, Duration::from_secs(1)).is_err());
        }
    }
    #[tokio::test]
    async fn api_roundtrip_keeps_existing_metric_identity_and_no_source_db() {
        let (url, task) = mock(success(), StatusCode::OK, Duration::ZERO).await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("quota-api.json");
        descriptor(&path, &url);
        let config = QuotaConfig {
            api_file: path,
            quota_db: dir.path().join("quota.db"),
            interval: Duration::from_secs(60),
            query_timeout: Duration::from_secs(2),
            upload_batch_size: 512,
        };
        let collected = collect_cycle(&config).await.unwrap();
        assert_eq!(collected[0].state.status, QuotaProviderStatus::Ok);
        assert_eq!(
            collected[0].observation.as_ref().unwrap().metrics[0].key,
            "subscription:five-hour"
        );
        assert_eq!(persist_cycle(&config.quota_db, &collected).unwrap(), 1);
        task.abort();
        let failed = collect_cycle(&config).await.unwrap();
        assert_eq!(failed.len(), 1);
        assert!(failed[0].observation.is_none());
        assert_eq!(
            failed[0].state.diagnostic_code.as_deref(),
            Some("quota_api_unavailable")
        );
    }
    #[tokio::test]
    async fn failures_never_become_samples_and_restarts_rediscover_endpoint() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("quota-api.json");
        let provider = CodexProvider {
            id: "provider-a".into(),
            name: "A".into(),
        };
        let mut wrong_version = success();
        wrong_version["schemaVersion"] = json!(2);
        let mut wrong_provider = success();
        wrong_provider["providerId"] = json!("other");
        for (value, status, delay) in [
            (success(), StatusCode::UNAUTHORIZED, Duration::ZERO),
            (success(), StatusCode::FOUND, Duration::ZERO),
            (wrong_version, StatusCode::OK, Duration::ZERO),
            (wrong_provider, StatusCode::OK, Duration::ZERO),
            (
                json!({"large":"x".repeat(MAX_API_RESPONSE_BYTES + 1)}),
                StatusCode::OK,
                Duration::ZERO,
            ),
            (success(), StatusCode::OK, Duration::from_millis(150)),
        ] {
            let (url, task) = mock(value, status, delay).await;
            descriptor(&path, &url);
            let api = QuotaApi::connect(&path, Duration::from_millis(100))
                .ok()
                .unwrap();
            assert!(api.query(&provider).await.observation.is_none());
            task.abort();
        }
        let (url, task) = mock(success(), StatusCode::OK, Duration::ZERO).await;
        descriptor(&path, &url);
        let api = QuotaApi::connect(&path, Duration::from_secs(2))
            .ok()
            .unwrap();
        assert!(api.query(&provider).await.observation.is_some());
        task.abort();
    }
}
