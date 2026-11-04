use crate::{Error, dto::*};
use reqwest::{Method, Url};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::{fmt, sync::Arc, time::Duration};
use tokio::{
    sync::Mutex,
    time::{Instant, timeout_at},
};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Environment {
    #[default]
    Sandbox,
    Production,
}
impl Environment {
    fn base_url(self) -> &'static str {
        match self {
            Self::Sandbox => "https://baas-api-sandbox.c6bank.info",
            Self::Production => "https://baas-api.c6bank.info",
        }
    }
}

/// Clones share the connection pool and one expiry-aware token cache.
#[derive(Clone)]
pub struct Client(Arc<Inner>);
struct Inner {
    http: reqwest::Client,
    base_url: Url,
    auth_url: Url,
    client_id: String,
    client_secret: String,
    token: Mutex<Option<Arc<Token>>>,
    operation_timeout: Duration,
}
struct ResponseContext {
    response: reqwest::Response,
    deadline: Instant,
    mutation: bool,
}

struct Token {
    value: String,
    expires_at: Instant,
}

/// Provide an mTLS identity, or inject an HTTP client that already has one.
pub struct ClientBuilder {
    client_id: String,
    client_secret: String,
    base_url: Url,
    auth_url: Option<Url>,
    identity: Option<reqwest::Identity>,
    root_certificates: Vec<reqwest::Certificate>,
    http: Option<reqwest::Client>,
    timeout: Duration,
    operation_timeout: Duration,
}
impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client").finish_non_exhaustive()
    }
}
impl fmt::Debug for ClientBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClientBuilder").finish_non_exhaustive()
    }
}
impl ClientBuilder {
    pub fn new(client_id: impl Into<String>, client_secret: impl Into<String>) -> Self {
        Self {
            client_id: client_id.into(),
            client_secret: client_secret.into(),
            base_url: Url::parse(Environment::Sandbox.base_url()).expect("static URL"),
            auth_url: None,
            identity: None,
            root_certificates: vec![],
            http: None,
            timeout: Duration::from_secs(30),
            operation_timeout: Duration::from_secs(60),
        }
    }
    pub fn environment(mut self, environment: Environment) -> Self {
        self.base_url = Url::parse(environment.base_url()).expect("static URL");
        self
    }
    /// Origin/base path override, primarily for local transport tests.
    pub fn base_url(mut self, base_url: &str) -> Result<Self, Error> {
        self.base_url = parse_url(base_url)?;
        Ok(self)
    }
    pub fn auth_url(mut self, auth_url: &str) -> Result<Self, Error> {
        self.auth_url = Some(parse_url(auth_url)?);
        Ok(self)
    }
    /// Loads PEM certificate chain and an unencrypted PEM private key.
    pub fn identity_pem(mut self, certificate: &[u8], private_key: &[u8]) -> Result<Self, Error> {
        let mut pem = Vec::with_capacity(certificate.len() + private_key.len() + 1);
        pem.extend_from_slice(certificate);
        pem.push(b'\n');
        pem.extend_from_slice(private_key);
        self.identity = Some(reqwest::Identity::from_pem(&pem).map_err(|_| Error::Configuration)?);
        Ok(self)
    }
    /// Adds a trusted PEM certificate (for private PKI or local TLS tests).
    pub fn add_root_certificate_pem(mut self, certificate: &[u8]) -> Result<Self, Error> {
        self.root_certificates
            .push(reqwest::Certificate::from_pem(certificate).map_err(|_| Error::Configuration)?);
        Ok(self)
    }
    /// The caller controls TLS, redirects, retries and timeouts on injected clients.
    /// This replaces the transport settings configured through this builder.
    pub fn http_client(mut self, http: reqwest::Client) -> Self {
        self.http = Some(http);
        self
    }
    /// HTTP-attempt timeout for the SDK-built transport (default 30 seconds).
    /// An injected HTTP client retains its own transport policy.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
    /// Complete operation budget, including token-lock wait, OAuth and response
    /// decoding (default 60 seconds). Applies to injected HTTP clients too.
    pub fn operation_timeout(mut self, timeout: Duration) -> Self {
        self.operation_timeout = timeout;
        self
    }
    pub fn build(self) -> Result<Client, Error> {
        validate_timeout(self.operation_timeout)?;
        if self.client_id.is_empty() || self.client_secret.is_empty() {
            return Err(Error::Configuration);
        }
        let http = match self.http {
            Some(http) => http,
            None => {
                validate_timeout(self.timeout)?;
                let mut builder = reqwest::Client::builder()
                    .tls_backend_rustls()
                    .identity(self.identity.ok_or(Error::Configuration)?)
                    .redirect(reqwest::redirect::Policy::none())
                    .retry(reqwest::retry::never())
                    .timeout(self.timeout);
                for certificate in self.root_certificates {
                    builder = builder.add_root_certificate(certificate);
                }
                builder.build().map_err(|_| Error::Configuration)?
            }
        };
        let auth_url = match self.auth_url {
            Some(url) => url,
            None => endpoint(&self.base_url, &["v1", "auth", ""])?,
        };
        Ok(Client(Arc::new(Inner {
            http,
            base_url: self.base_url,
            auth_url,
            client_id: self.client_id,
            client_secret: self.client_secret,
            token: Mutex::new(None),
            operation_timeout: self.operation_timeout,
        })))
    }
}
fn validate_timeout(timeout: Duration) -> Result<(), Error> {
    if timeout.is_zero() || Instant::now().checked_add(timeout).is_none() {
        return Err(Error::Configuration);
    }
    Ok(())
}

fn parse_url(value: &str) -> Result<Url, Error> {
    let url = Url::parse(value).map_err(|_| Error::Configuration)?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(Error::Configuration);
    }
    Ok(url)
}
fn endpoint(base: &Url, parts: &[&str]) -> Result<Url, Error> {
    let mut url = base.clone();
    {
        let mut segments = url.path_segments_mut().map_err(|_| Error::Configuration)?;
        segments.pop_if_empty();
        for part in parts {
            if *part == "." || *part == ".." {
                return Err(Error::Configuration);
            }
            segments.push(part);
        }
    }
    Ok(url)
}

fn retry_after(response: &reqwest::Response) -> Option<String> {
    response
        .headers()
        .get("Retry-After")
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned)
}

impl Client {
    pub fn builder(
        client_id: impl Into<String>,
        client_secret: impl Into<String>,
    ) -> ClientBuilder {
        ClientBuilder::new(client_id, client_secret)
    }
    async fn token(&self, deadline: Instant) -> Result<Arc<Token>, Error> {
        let unavailable = || Error::Authentication {
            status: None,
            retry_after: None,
        };
        let mut cache = timeout_at(deadline, self.0.token.lock())
            .await
            .map_err(|_| unavailable())?;
        if Instant::now() >= deadline {
            return Err(unavailable());
        }
        if let Some(token) = cache
            .as_ref()
            .filter(|token| token.expires_at > Instant::now())
        {
            return Ok(token.clone());
        }
        #[derive(Deserialize)]
        struct Response {
            access_token: String,
            expires_in: u64,
        }
        let started = Instant::now();
        let response = timeout_at(
            deadline,
            self.0
                .http
                .post(self.0.auth_url.clone())
                .form(&[
                    ("grant_type", "client_credentials"),
                    ("client_id", self.0.client_id.as_str()),
                    ("client_secret", self.0.client_secret.as_str()),
                ])
                .send(),
        )
        .await
        .map_err(|_| unavailable())?
        .map_err(|_| unavailable())?;
        let authentication = Error::Authentication {
            status: Some(response.status().as_u16()),
            retry_after: retry_after(&response),
        };
        if !response.status().is_success() {
            return Err(authentication);
        }
        let response: Response = read_json(response, 65_536, deadline)
            .await
            .map_err(|_| authentication.clone())?;
        if response.access_token.is_empty() {
            return Err(authentication);
        }
        let ttl = Duration::from_secs(response.expires_in);
        let margin = (ttl / 10).min(Duration::from_secs(30));
        let expires_at = started
            .checked_add(ttl.saturating_sub(margin))
            .ok_or(authentication)?;
        let token = Arc::new(Token {
            value: response.access_token,
            expires_at,
        });
        *cache = Some(token.clone());
        Ok(token)
    }
    async fn request<B: Serialize + ?Sized>(
        &self,
        method: Method,
        resource: &str,
        id: Option<&str>,
        body: Option<&B>,
        query: Option<&PixQuery>,
    ) -> Result<ResponseContext, Error> {
        let mutation = method != Method::GET;
        let mut parts = vec!["v2", "pix", resource];
        if let Some(id) = id {
            if id.is_empty() {
                return Err(Error::Configuration);
            }
            parts.push(id);
        }
        let url = endpoint(&self.0.base_url, &parts)?;
        let deadline = Instant::now()
            .checked_add(self.0.operation_timeout)
            .ok_or(Error::Configuration)?;
        let token = self.token(deadline).await?;
        let mut request = self.0.http.request(method, url).bearer_auth(&token.value);
        if let Some(body) = body {
            request = request.json(body);
        }
        if let Some(query) = query {
            request = request.query(query);
        }
        if Instant::now() >= deadline {
            return Err(Error::Authentication {
                status: None,
                retry_after: None,
            });
        }
        let response = timeout_at(deadline, request.send())
            .await
            .map_err(|_| Error::Transport {
                indeterminate: mutation,
            })?
            .map_err(|e| Error::Transport {
                indeterminate: mutation && !e.is_builder(),
            })?;
        let status = response.status();
        if status.as_u16() == 401 {
            // Keep the received 401 even if a concurrent refresh holds the lock
            // until this operation expires. Identity distinguishes a fresh token
            // even when the provider reuses the same access-token text.
            if let Ok(mut cache) = timeout_at(deadline, self.0.token.lock()).await {
                let rejected = cache
                    .as_ref()
                    .is_some_and(|cached| Arc::ptr_eq(cached, &token));
                if rejected {
                    *cache = None;
                }
            }
        }
        if !status.is_success() {
            return Err(Error::Http {
                status: status.as_u16(),
                retry_after: retry_after(&response),
                indeterminate: mutation && (status.is_server_error() || status.as_u16() == 408),
            });
        }
        if Instant::now() >= deadline {
            return Err(Error::Transport {
                indeterminate: mutation,
            });
        }
        Ok(ResponseContext {
            response,
            deadline,
            mutation,
        })
    }
    async fn decode<T: DeserializeOwned>(context: ResponseContext) -> Result<T, Error> {
        read_json(context.response, 10_485_760, context.deadline)
            .await
            .map_err(|_| Error::Decode {
                indeterminate: context.mutation,
            })
    }
    /// Creates/replaces the caller's immutable transaction ID; no automatic replay.
    pub async fn put_due_charge(
        &self,
        txid: &str,
        charge: &DueChargeRequest,
    ) -> Result<DueCharge, Error> {
        Self::decode(
            self.request(Method::PUT, "cobv", Some(txid), Some(charge), None)
                .await?,
        )
        .await
    }
    pub async fn get_due_charge(&self, txid: &str) -> Result<DueCharge, Error> {
        Self::decode(
            self.request::<Value>(Method::GET, "cobv", Some(txid), None, None)
                .await?,
        )
        .await
    }
    pub async fn patch_due_charge(&self, txid: &str, patch: &Value) -> Result<DueCharge, Error> {
        Self::decode(
            self.request(Method::PATCH, "cobv", Some(txid), Some(patch), None)
                .await?,
        )
        .await
    }
    pub async fn cancel_due_charge(&self, txid: &str) -> Result<DueCharge, Error> {
        self.patch_due_charge(txid, &json!({"status":"REMOVIDA_PELO_USUARIO_RECEBEDOR"}))
            .await
    }
    pub async fn get_pix(&self, e2eid: &str) -> Result<Pix, Error> {
        Self::decode(
            self.request::<Value>(Method::GET, "pix", Some(e2eid), None, None)
                .await?,
        )
        .await
    }
    /// Fetches one page. Increment `pagina_atual` only after durably processing it.
    /// Bounds are converted to UTC and fractional seconds are truncated. Persist
    /// polling checkpoints at whole-second precision to match the queried window.
    /// Invalid or reversed RFC3339 bounds fail before authentication or network IO.
    pub async fn list_pix(&self, query: &PixQuery) -> Result<PixPage, Error> {
        let query = query.normalized()?;
        Self::decode(
            self.request::<Value>(Method::GET, "pix", None, None, Some(&query))
                .await?,
        )
        .await
    }
    pub async fn put_webhook(&self, key: &str, url: &str) -> Result<(), Error> {
        self.request(
            Method::PUT,
            "webhook",
            Some(key),
            Some(&json!({"webhookUrl":url})),
            None,
        )
        .await?;
        Ok(())
    }
    pub async fn get_webhook(&self, key: &str) -> Result<Webhook, Error> {
        Self::decode(
            self.request::<Value>(Method::GET, "webhook", Some(key), None, None)
                .await?,
        )
        .await
    }
    pub async fn delete_webhook(&self, key: &str) -> Result<(), Error> {
        self.request::<Value>(Method::DELETE, "webhook", Some(key), None, None)
            .await?;
        Ok(())
    }
}

/// The limits bound the SDK's retained JSON bytes, not provider response maxima.
async fn read_json<T: DeserializeOwned>(
    mut response: reqwest::Response,
    limit: usize,
    deadline: Instant,
) -> Result<T, ()> {
    timeout_at(deadline, async {
        if response
            .content_length()
            .is_some_and(|size| size > limit as u64)
        {
            return Err(());
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| ())? {
            if Instant::now() >= deadline {
                return Err(());
            }
            let length = body
                .len()
                .checked_add(chunk.len())
                .filter(|size| *size <= limit)
                .ok_or(())?;
            if length > body.capacity() {
                let capacity = body.capacity().saturating_mul(2).max(length).min(limit);
                body.try_reserve_exact(capacity - body.len())
                    .map_err(|_| ())?;
            }
            body.extend_from_slice(&chunk);
        }
        // Parsing is synchronous and bounded by the retained bytes. Check on
        // both sides so even an immediately-ready future cannot succeed late.
        if Instant::now() >= deadline {
            return Err(());
        }
        let decoded = serde_json::from_slice(&body).map_err(|_| ())?;
        if Instant::now() >= deadline {
            return Err(());
        }
        Ok(decoded)
    })
    .await
    .map_err(|_| ())?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn lock_wait_expiry_is_authentication_and_releases_cancelled_waiter() {
        let client = Client::builder("id", "secret")
            .http_client(reqwest::Client::new())
            .base_url("http://127.0.0.1:1")
            .unwrap()
            .operation_timeout(Duration::from_millis(20))
            .build()
            .unwrap();
        let mut held = client.0.token.lock().await;
        let error = client.cancel_due_charge("charge").await.unwrap_err();
        assert_eq!(
            error,
            Error::Authentication {
                status: None,
                retry_after: None
            }
        );
        *held = Some(Arc::new(Token {
            value: "synthetic-token".into(),
            expires_at: Instant::now() + Duration::from_secs(60),
        }));
        drop(held);
        // A timed-out lock waiter cannot retain ownership or block later calls.
        assert_eq!(
            client
                .token(Instant::now() + Duration::from_millis(20))
                .await
                .unwrap()
                .value,
            "synthetic-token"
        );
    }
}
