//! Neon service wiring.
//!
//! The application talks to Lakebase Postgres through SeaORM (see `main.rs`).
//! This module covers the other Neon branch services that are useful to a
//! single-binary Axum server:
//!
//! * **Functions** - a plain HTTPS call to the deployed `hello` function.
//! * **Object Storage** - a small, dependency-free S3 client (SigV4, path-style)
//!   for the branch bucket declared in `neon.ts`.
//! * **Realtime** - sealed live queries for the current guest's transcript,
//!   using the same compact-JWE format as `@neon/realtime/server`. The browser
//!   consumes the sealed capability with the official JavaScript SDK.
//!
//! No credentials are ever logged or returned by an HTTP handler.

use std::time::Duration;

use aes_gcm::{
    aead::{Aead, AeadCore, KeyInit, OsRng, Payload},
    Aes256Gcm,
};
use base64::{
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
    Engine,
};
use hmac::{Hmac, Mac};
use percent_encoding::{utf8_percent_encode, AsciiSet, NON_ALPHANUMERIC};
use reqwest::{Client, Method, StatusCode, Url};
use serde::Serialize;
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

const REALTIME_SECRET_PREFIX: &str = "nrt_live_1";
const REALTIME_SECRET_KEY_BYTES: usize = 32;
const REALTIME_TOKEN_LIFETIME_SECONDS: i64 = 60;
const REALTIME_IV_BYTES: usize = 12;
const REALTIME_TAG_BYTES: usize = 16;
const REALTIME_FINGERPRINT_DOMAIN: &[u8] = b"neon-live-query-fingerprint-v1\0";

/// The transcript query is fixed so the sealed capability can never be used to
/// read another table or another guest's conversation.
const USER_MESSAGES_SQL: &str = "SELECT id, user_id, role, content, created_at \
FROM chat_messages WHERE user_id = $1 ORDER BY id";

/// Percent-encode set for AWS SigV4: everything except unreserved characters.
const AWS_ENCODE_SET: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'~');

#[cfg(test)]
const EMPTY_PAYLOAD_SHA256: &str =
    "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

#[derive(Clone)]
pub struct NeonServices {
    pub function_hello_url: Option<String>,
    pub storage: Option<StorageConfig>,
    pub realtime: Option<RealtimeConfig>,
    pub http: Client,
}

#[derive(Clone)]
pub struct StorageConfig {
    pub endpoint: String,
    pub region: String,
    pub access_key: String,
    pub secret_key: String,
    pub bucket: String,
    pub http: Client,
}

/// Everything needed to mint a short-lived sealed live query. The secret is
/// kept private: it can encrypt capabilities and must never be served.
#[derive(Clone)]
pub struct RealtimeConfig {
    url: String,
    database: String,
    secret_key: [u8; REALTIME_SECRET_KEY_BYTES],
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SealedLiveQuery {
    pub capability: String,
    pub query_fingerprint: String,
    pub expires_at: i64,
    pub url: String,
    pub database: String,
}

#[derive(Debug, Serialize)]
pub struct RealtimeSummary {
    pub configured: bool,
    pub url: Option<String>,
    pub database: Option<String>,
    pub secret_configured: bool,
}

#[derive(Serialize)]
struct LiveQueryParameter {
    type_oid: u32,
    value: Option<String>,
}

#[derive(Serialize)]
struct QueryCanonical<'a> {
    sql: &'a str,
    parameters: &'a [LiveQueryParameter],
}

#[derive(Serialize)]
struct LiveQueryClaims<'a> {
    v: u8,
    aud: &'static str,
    database: &'a str,
    error_details: &'static str,
    query_fingerprint: &'a str,
    sql: &'a str,
    parameters: &'a [LiveQueryParameter],
    iat: i64,
    exp: i64,
}

#[derive(Serialize)]
struct LiveQueryHeader {
    alg: &'static str,
    enc: &'static str,
    v: u8,
}

#[derive(Debug, Serialize)]
pub struct StorageObject {
    pub key: String,
    pub size: u64,
    pub last_modified: Option<String>,
}

impl NeonServices {
    pub fn new(
        function_hello_url: Option<String>,
        storage: Option<StorageConfig>,
        realtime: Option<RealtimeConfig>,
    ) -> Self {
        Self {
            function_hello_url,
            storage,
            realtime,
            http: Client::builder()
                .timeout(Duration::from_secs(15))
                .build()
                .expect("reqwest client"),
        }
    }

    pub fn realtime_summary(&self) -> RealtimeSummary {
        match &self.realtime {
            Some(realtime) => RealtimeSummary {
                configured: true,
                url: Some(realtime.url.clone()),
                database: Some(realtime.database.clone()),
                secret_configured: true,
            },
            None => RealtimeSummary {
                configured: false,
                url: None,
                database: None,
                secret_configured: false,
            },
        }
    }

    /// Call the deployed Neon Function and return its status and body.
    pub async fn call_hello_function(&self) -> Result<(u16, String), String> {
        let url = self
            .function_hello_url
            .as_deref()
            .ok_or_else(|| "NEON_FUNCTION_HELLO_BASE_URL is not set".to_string())?;
        let response = self
            .http
            .get(url)
            .send()
            .await
            .map_err(|err| format!("Neon Function request failed: {err}"))?;
        let status = response.status().as_u16();
        let body = response
            .text()
            .await
            .map_err(|err| format!("Neon Function response could not be read: {err}"))?;
        Ok((status, truncate(&body, 500)))
    }
}

impl RealtimeConfig {
    /// Read the branch Realtime URL and server-only secret injected by
    /// `neon deploy`. The database name comes from the Postgres URL because a
    /// capability is bound to one database.
    pub fn from_env(database_url: &str) -> Result<Option<Self>, String> {
        let url = optional_env("NEON_REALTIME_URL");
        let secret = optional_env("NEON_REALTIME_SECRET");

        match (&url, &secret) {
            (None, None) => Ok(None),
            (Some(_), None) => Err(
                "NEON_REALTIME_URL is set but NEON_REALTIME_SECRET is missing. \
Run `neon env pull` or `neon deploy` to refresh the branch variables."
                    .to_string(),
            ),
            (None, Some(_)) => Err(
                "NEON_REALTIME_SECRET is set but NEON_REALTIME_URL is missing. \
Run `neon env pull` or `neon deploy` to refresh the branch variables."
                    .to_string(),
            ),
            (Some(url), Some(secret)) => {
                let parsed = Url::parse(url)
                    .map_err(|err| format!("NEON_REALTIME_URL is not a valid URL: {err}"))?;
                if !matches!(parsed.scheme(), "ws" | "wss") {
                    return Err("NEON_REALTIME_URL must use ws:// or wss://.".to_string());
                }
                let secret_key = parse_realtime_secret(secret)?;
                let database = database_name(database_url)?;
                Ok(Some(Self {
                    url: url.clone(),
                    database,
                    secret_key,
                }))
            }
        }
    }

    /// Seal the guest-transcript query as a compact JWE that the Realtime
    /// proxy accepts. The capability is short-lived and only readable by the
    /// holder; the shared secret never leaves the server.
    pub fn seal_user_messages(&self, user_id: &str) -> Result<SealedLiveQuery, String> {
        let parameters = vec![LiveQueryParameter {
            type_oid: 0,
            value: Some(STANDARD.encode(user_id.as_bytes())),
        }];

        let query_fingerprint = query_fingerprint(USER_MESSAGES_SQL, &parameters);
        let issued_at = chrono::Utc::now().timestamp();
        let expires_at = issued_at + REALTIME_TOKEN_LIFETIME_SECONDS;

        let claims = LiveQueryClaims {
            v: 1,
            aud: "realtime-proxy",
            database: &self.database,
            error_details: "safe",
            query_fingerprint: &query_fingerprint,
            sql: USER_MESSAGES_SQL,
            parameters: &parameters,
            iat: issued_at,
            exp: expires_at,
        };
        let claims_json = serde_json::to_vec(&claims)
            .map_err(|err| format!("realtime claims could not be serialised: {err}"))?;

        let header = LiveQueryHeader {
            alg: "dir",
            enc: "A256GCM",
            v: 1,
        };
        let header_json = serde_json::to_vec(&header)
            .map_err(|err| format!("realtime header could not be serialised: {err}"))?;
        let encoded_header = URL_SAFE_NO_PAD.encode(&header_json);

        let cipher = Aes256Gcm::new_from_slice(&self.secret_key)
            .map_err(|_| "realtime secret could not initialise AES-256-GCM".to_string())?;
        let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
        let sealed = cipher
            .encrypt(
                &nonce,
                Payload {
                    msg: &claims_json,
                    aad: encoded_header.as_bytes(),
                },
            )
            .map_err(|_| "realtime capability encryption failed".to_string())?;
        if sealed.len() < REALTIME_TAG_BYTES {
            return Err("realtime capability encryption returned no tag".to_string());
        }
        let (ciphertext, tag) = sealed.split_at(sealed.len() - REALTIME_TAG_BYTES);
        debug_assert_eq!(nonce.len(), REALTIME_IV_BYTES);

        let capability = format!(
            "{encoded_header}..{}.{}.{}",
            URL_SAFE_NO_PAD.encode(nonce),
            URL_SAFE_NO_PAD.encode(ciphertext),
            URL_SAFE_NO_PAD.encode(tag)
        );

        Ok(SealedLiveQuery {
            capability,
            query_fingerprint,
            expires_at: expires_at * 1_000,
            url: self.url.clone(),
            database: self.database.clone(),
        })
    }
}

fn parse_realtime_secret(raw: &str) -> Result<[u8; REALTIME_SECRET_KEY_BYTES], String> {
    let encoded = raw
        .trim()
        .strip_prefix(REALTIME_SECRET_PREFIX)
        .ok_or_else(|| "NEON_REALTIME_SECRET is not a Neon Realtime secret.".to_string())?;
    if encoded.is_empty() || !encoded.bytes().all(is_base64_url_byte) {
        return Err("NEON_REALTIME_SECRET is not valid base64url.".to_string());
    }
    let decoded = URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| "NEON_REALTIME_SECRET is not valid base64url.".to_string())?;
    decoded.try_into().map_err(|_| {
        format!(
            "NEON_REALTIME_SECRET must contain a {REALTIME_SECRET_KEY_BYTES}-byte key; \
rerun `neon deploy` to refresh it."
        )
    })
}

fn is_base64_url_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_'
}

fn database_name(database_url: &str) -> Result<String, String> {
    let url = Url::parse(database_url)
        .map_err(|err| format!("DATABASE_URL is not a valid URL: {err}"))?;
    let database = url.path().trim_matches('/');
    if database.is_empty() {
        return Err(
            "DATABASE_URL must include a database name for Neon Realtime to bind to.".to_string(),
        );
    }
    if database.contains('/') {
        return Err("DATABASE_URL must contain a single database name.".to_string());
    }
    Ok(percent_encoding::percent_decode_str(database)
        .decode_utf8_lossy()
        .to_string())
}

/// Stored values are the base64 of the UTF-8 text, matching the JavaScript
/// capability issuer byte for byte. Property order is significant because the
/// fingerprint is computed over the JSON representation.
fn query_fingerprint(sql: &str, parameters: &[LiveQueryParameter]) -> String {
    let canonical = QueryCanonical { sql, parameters };
    let json = serde_json::to_vec(&canonical).expect("live-query canonical form");
    let mut hasher = Sha256::new();
    hasher.update(REALTIME_FINGERPRINT_DOMAIN);
    hasher.update(&json);
    hex(&hasher.finalize())
}

impl StorageConfig {
    /// Read the AWS-standard variables injected by `neon deploy`.
    pub fn from_env() -> Result<Option<Self>, String> {
        let access_key = optional_env("AWS_ACCESS_KEY_ID");
        let secret_key = optional_env("AWS_SECRET_ACCESS_KEY");
        let endpoint = optional_env("AWS_ENDPOINT_URL_S3");
        let region = optional_env("AWS_REGION");

        let configured = [&access_key, &secret_key, &endpoint, &region]
            .into_iter()
            .filter(|value| value.is_some())
            .count();

        if configured == 0 {
            return Ok(None);
        }
        if configured != 4 {
            return Err(
                "Object storage is partially configured. Set AWS_ACCESS_KEY_ID, \
AWS_SECRET_ACCESS_KEY, AWS_ENDPOINT_URL_S3 and AWS_REGION together (normally via `neon deploy`/`neon env pull`)."
                    .to_string(),
            );
        }

        let endpoint = endpoint.expect("checked above");
        Url::parse(&endpoint)
            .map_err(|err| format!("AWS_ENDPOINT_URL_S3 is not a valid URL: {err}"))?;

        Ok(Some(Self {
            endpoint,
            region: region.expect("checked above"),
            access_key: access_key.expect("checked above"),
            secret_key: secret_key.expect("checked above"),
            bucket: optional_env("NEON_STORAGE_BUCKET").unwrap_or_else(|| "assets".to_string()),
            http: Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .map_err(|err| format!("object-storage HTTP client could not be built: {err}"))?,
        }))
    }

    /// Confirm the credentials and endpoint can reach the branch bucket.
    pub async fn head_bucket(&self) -> Result<(), String> {
        let url = self.bucket_url()?;
        self.send(Method::HEAD, url, Vec::new(), None).await?;
        Ok(())
    }

    pub async fn list(&self, prefix: &str, max_keys: u32) -> Result<Vec<StorageObject>, String> {
        let mut url = self.bucket_url()?;
        {
            let mut query = url.query_pairs_mut();
            query.append_pair("list-type", "2");
            query.append_pair("max-keys", &max_keys.to_string());
            if !prefix.is_empty() {
                query.append_pair("prefix", prefix);
            }
        }
        let body = self.send(Method::GET, url, Vec::new(), None).await?;
        Ok(parse_list_objects(&String::from_utf8_lossy(&body)))
    }

    pub async fn put(&self, key: &str, body: Vec<u8>, content_type: &str) -> Result<(), String> {
        let url = self.object_url(key)?;
        self.send(Method::PUT, url, body, Some(content_type))
            .await?;
        Ok(())
    }

    pub async fn get(&self, key: &str) -> Result<(Vec<u8>, String), String> {
        let url = self.object_url(key)?;
        let (status, headers, body) = self.send_raw(Method::GET, url, Vec::new(), None).await?;
        if status == StatusCode::NOT_FOUND {
            return Err("object not found".to_string());
        }
        if !status.is_success() {
            return Err(format!("S3 returned {status}"));
        }
        let content_type = headers
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("application/octet-stream")
            .to_string();
        Ok((body, content_type))
    }

    pub async fn delete(&self, key: &str) -> Result<(), String> {
        let url = self.object_url(key)?;
        self.send(Method::DELETE, url, Vec::new(), None).await?;
        Ok(())
    }

    fn bucket_url(&self) -> Result<Url, String> {
        let mut url = Url::parse(self.endpoint.trim_end_matches('/'))
            .map_err(|err| format!("AWS_ENDPOINT_URL_S3 is not a valid URL: {err}"))?;
        {
            let mut segments = url
                .path_segments_mut()
                .map_err(|_| "AWS_ENDPOINT_URL_S3 cannot be used as a base URL".to_string())?;
            segments.pop_if_empty();
            segments.push(&self.bucket);
        }
        Ok(url)
    }

    fn object_url(&self, key: &str) -> Result<Url, String> {
        if !valid_key(key) {
            return Err(
                "object key must use only letters, digits, '/', '-', '_', '.' and '~', \
must not contain empty or '..' segments, and must be at most 512 characters"
                    .to_string(),
            );
        }
        let mut url = self.bucket_url()?;
        {
            let mut segments = url
                .path_segments_mut()
                .map_err(|_| "AWS_ENDPOINT_URL_S3 cannot be used as a base URL".to_string())?;
            for segment in key.split('/') {
                segments.push(segment);
            }
        }
        Ok(url)
    }

    async fn send(
        &self,
        method: Method,
        url: Url,
        body: Vec<u8>,
        content_type: Option<&str>,
    ) -> Result<Vec<u8>, String> {
        let (status, _, body) = self.send_raw(method, url, body, content_type).await?;
        if !status.is_success() {
            return Err(format!(
                "S3 returned {status}: {}",
                truncate(&body_text(&body), 300)
            ));
        }
        Ok(body)
    }

    async fn send_raw(
        &self,
        method: Method,
        url: Url,
        body: Vec<u8>,
        content_type: Option<&str>,
    ) -> Result<(StatusCode, reqwest::header::HeaderMap, Vec<u8>), String> {
        let payload_hash = sha256_hex(&body);
        let signed_headers = self.signed_headers(&method, &url, &payload_hash, content_type)?;

        let mut request = self
            .http
            .request(method, url)
            .timeout(Duration::from_secs(30));
        for (name, value) in signed_headers {
            request = request.header(name, value);
        }
        if !body.is_empty() || content_type.is_some() {
            request = request.body(body);
        }

        let response = request
            .send()
            .await
            .map_err(|err| format!("S3 request failed: {err}"))?;
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = response
            .bytes()
            .await
            .map_err(|err| format!("S3 response could not be read: {err}"))?;
        Ok((status, headers, bytes.to_vec()))
    }

    fn signed_headers(
        &self,
        method: &Method,
        url: &Url,
        payload_hash: &str,
        content_type: Option<&str>,
    ) -> Result<Vec<(&'static str, String)>, String> {
        let now = chrono::Utc::now();
        let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
        let date = now.format("%Y%m%d").to_string();

        let host = host_header(url)?;
        let mut headers: Vec<(&'static str, String)> = vec![
            ("host", host),
            ("x-amz-content-sha256", payload_hash.to_string()),
            ("x-amz-date", amz_date.clone()),
        ];
        if let Some(value) = content_type {
            headers.push(("content-type", value.to_string()));
        }
        headers.sort_by(|left, right| left.0.cmp(right.0));

        let canonical_headers = headers
            .iter()
            .map(|(name, value)| format!("{name}:{}\n", value.trim()))
            .collect::<String>();
        let signed_header_names = headers
            .iter()
            .map(|(name, _)| *name)
            .collect::<Vec<_>>()
            .join(";");

        let canonical_uri = canonical_uri(url);
        let canonical_query = canonical_query(url);

        let canonical_request = format!(
            "{}\n{}\n{}\n{}\n{}\n{}",
            method.as_str(),
            canonical_uri,
            canonical_query,
            canonical_headers,
            signed_header_names,
            payload_hash
        );

        let scope = format!("{date}/{}/s3/aws4_request", self.region);
        let string_to_sign = format!(
            "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
            sha256_hex(canonical_request.as_bytes())
        );

        let signing_key = signing_key(&self.secret_key, &date, &self.region, "s3");
        let signature = hex(&hmac_sha256(&signing_key, string_to_sign.as_bytes()));
        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_header_names}, Signature={signature}",
            self.access_key
        );

        let mut result = Vec::with_capacity(headers.len() + 1);
        // `host` is set by reqwest from the URL; adding it again is harmless but
        // unnecessary. Keep it out of the request headers.
        for (name, value) in headers {
            if name == "host" {
                continue;
            }
            result.push((name, value));
        }
        result.push(("authorization", authorization));
        Ok(result)
    }
}

/// A compact object-key guard. Restricting characters keeps the URL path,
/// the SigV4 canonical URI, and the S3 key byte-for-byte identical.
pub fn valid_key(key: &str) -> bool {
    if key.is_empty() || key.len() > 512 || key.starts_with('/') {
        return false;
    }
    let mut segments = key.split('/');
    if segments.any(|segment| segment.is_empty() || segment == "..") {
        return false;
    }
    key.chars().all(|character| {
        character.is_ascii_alphanumeric() || matches!(character, '/' | '-' | '_' | '.' | '~')
    })
}

pub fn valid_prefix(prefix: &str) -> bool {
    if prefix.is_empty() {
        return true;
    }
    if prefix.len() > 512 || prefix.starts_with('/') {
        return false;
    }
    prefix.chars().all(|character| {
        character.is_ascii_alphanumeric() || matches!(character, '/' | '-' | '_' | '.' | '~')
    })
}

fn optional_env(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn host_header(url: &Url) -> Result<String, String> {
    let host = url
        .host_str()
        .ok_or_else(|| "S3 endpoint has no host".to_string())?;
    match url.port() {
        Some(port) if !is_default_port(url.scheme(), port) => Ok(format!("{host}:{port}")),
        _ => Ok(host.to_string()),
    }
}

fn is_default_port(scheme: &str, port: u16) -> bool {
    (scheme == "https" && port == 443) || (scheme == "http" && port == 80)
}

fn canonical_uri(url: &Url) -> String {
    let path = url.path();
    if path.is_empty() {
        "/".to_string()
    } else {
        path.to_string()
    }
}

fn canonical_query(url: &Url) -> String {
    let mut pairs: Vec<(String, String)> = url
        .query_pairs()
        .map(|(key, value)| (aws_encode(&key), aws_encode(&value)))
        .collect();
    pairs.sort();
    pairs
        .into_iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join("&")
}

fn aws_encode(value: &str) -> String {
    utf8_percent_encode(value, AWS_ENCODE_SET).to_string()
}

fn signing_key(secret: &str, date: &str, region: &str, service: &str) -> Vec<u8> {
    let date_key = hmac_sha256(format!("AWS4{secret}").as_bytes(), date.as_bytes());
    let region_key = hmac_sha256(&date_key, region.as_bytes());
    let service_key = hmac_sha256(&region_key, service.as_bytes());
    hmac_sha256(&service_key, b"aws4_request")
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = <HmacSha256 as Mac>::new_from_slice(key).expect("hmac accepts any key length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

fn sha256_hex(data: &[u8]) -> String {
    hex(&Sha256::digest(data))
}

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn body_text(body: &[u8]) -> String {
    String::from_utf8_lossy(body).to_string()
}

fn truncate(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    let mut shortened: String = value.chars().take(max_chars).collect();
    shortened.push('…');
    shortened
}

fn parse_list_objects(xml: &str) -> Vec<StorageObject> {
    let mut objects = Vec::new();
    let mut rest = xml;

    while let Some(start) = rest.find("<Contents>") {
        let after_start = &rest[start + "<Contents>".len()..];
        let end = after_start.find("</Contents>").unwrap_or(after_start.len());
        let block = &after_start[..end];

        if let Some(key) = extract_tag(block, "Key") {
            objects.push(StorageObject {
                key: xml_unescape(&key),
                size: extract_tag(block, "Size")
                    .and_then(|size| size.parse::<u64>().ok())
                    .unwrap_or(0),
                last_modified: extract_tag(block, "LastModified").map(|value| xml_unescape(&value)),
            });
        }

        rest = &after_start[end..];
    }

    objects
}

fn extract_tag(block: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = block.find(&open)? + open.len();
    let end = block[start..].find(&close)? + start;
    Some(block[start..end].to_string())
}

fn xml_unescape(value: &str) -> String {
    value
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_safe_keys_and_rejects_escapes() {
        assert!(valid_key("chat/2026/hello.txt"));
        assert!(valid_key("a-b_c.d~e"));
        assert!(!valid_key(""));
        assert!(!valid_key("/absolute"));
        assert!(!valid_key("a//b"));
        assert!(!valid_key("a/../b"));
        assert!(!valid_key("a b"));
        assert!(!valid_key("a?b"));
    }

    #[test]
    fn canonical_query_is_sorted() {
        let url =
            Url::parse("https://example.com/bucket?prefix=z&list-type=2&max-keys=10").unwrap();
        assert_eq!(canonical_query(&url), "list-type=2&max-keys=10&prefix=z");
    }

    #[test]
    fn parses_list_objects_xml() {
        let xml = r#"<?xml version="1.0"?><ListBucketResult>
            <Contents><Key>a/one.txt</Key><Size>3</Size><LastModified>2026-01-01T00:00:00Z</LastModified></Contents>
            <Contents><Key>a/two &amp; three.txt</Key><Size>7</Size></Contents>
        </ListBucketResult>"#;
        let objects = parse_list_objects(xml);
        assert_eq!(objects.len(), 2);
        assert_eq!(objects[0].key, "a/one.txt");
        assert_eq!(objects[0].size, 3);
        assert_eq!(objects[1].key, "a/two & three.txt");
    }

    #[test]
    fn empty_payload_hash_matches_constant() {
        assert_eq!(sha256_hex(b""), EMPTY_PAYLOAD_SHA256);
    }

    #[test]
    fn parses_neon_realtime_secret() {
        let encoded = URL_SAFE_NO_PAD.encode([9u8; REALTIME_SECRET_KEY_BYTES]);
        let secret = format!("{REALTIME_SECRET_PREFIX}{encoded}");
        assert_eq!(
            parse_realtime_secret(&secret).unwrap(),
            [9u8; REALTIME_SECRET_KEY_BYTES]
        );
        assert!(parse_realtime_secret("nrt_live_1short").is_err());
        assert!(parse_realtime_secret("missing-prefix").is_err());
    }

    #[test]
    fn seals_a_decryptable_live_query() {
        let key = [7u8; REALTIME_SECRET_KEY_BYTES];
        let config = RealtimeConfig {
            url: "wss://example.test".to_string(),
            database: "neondb".to_string(),
            secret_key: key,
        };
        let user_id = "208101b2-e13a-4351-8742-5d1f0b8bc9c6";
        let sealed = config.seal_user_messages(user_id).unwrap();

        let parts: Vec<&str> = sealed.capability.split('.').collect();
        assert_eq!(parts.len(), 5);
        assert_eq!(parts[1], "");

        let header = URL_SAFE_NO_PAD.decode(parts[0]).unwrap();
        assert_eq!(
            std::str::from_utf8(&header).unwrap(),
            r#"{"alg":"dir","enc":"A256GCM","v":1}"#
        );

        let nonce = URL_SAFE_NO_PAD.decode(parts[2]).unwrap();
        let ciphertext = URL_SAFE_NO_PAD.decode(parts[3]).unwrap();
        let tag = URL_SAFE_NO_PAD.decode(parts[4]).unwrap();
        let mut combined = ciphertext;
        combined.extend_from_slice(&tag);

        let cipher = Aes256Gcm::new_from_slice(&key).unwrap();
        let plaintext = cipher
            .decrypt(
                aes_gcm::Nonce::from_slice(&nonce),
                Payload {
                    msg: &combined,
                    aad: parts[0].as_bytes(),
                },
            )
            .unwrap();
        let claims: serde_json::Value = serde_json::from_slice(&plaintext).unwrap();
        assert_eq!(claims["aud"], "realtime-proxy");
        assert_eq!(claims["database"], "neondb");
        assert_eq!(claims["sql"], USER_MESSAGES_SQL);
        assert_eq!(claims["parameters"][0]["type_oid"].as_u64(), Some(0));
        assert_eq!(claims["parameters"][0]["value"], STANDARD.encode(user_id));
        assert_eq!(sealed.query_fingerprint.len(), 64);
        assert!(sealed.expires_at > chrono::Utc::now().timestamp_millis());
    }

    #[test]
    fn fingerprint_matches_known_javascript_output() {
        // Produced by @neon/realtime's queryFingerprint for the same SQL,
        // parameter, and insertion order.
        let parameters = vec![LiveQueryParameter {
            type_oid: 0,
            value: Some(STANDARD.encode("abc")),
        }];
        assert_eq!(
            query_fingerprint(USER_MESSAGES_SQL, &parameters),
            "083a0500597489078f20d8d00a8adc25e741806fb275991485f67da398b03574"
        );
    }
}
