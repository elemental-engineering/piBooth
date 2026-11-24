//! Minimal Immich API client (blocking; used from the upload thread only).
//! Field names follow the server's OpenAPI spec: `deviceAssetId`/`deviceId`
//! are required up to Immich 2.x and ignored by 3.x, so they are always sent.

use std::path::Path;
use std::time::Duration;

use serde::Deserialize;
use ureq::Agent;
use ureq::tls::{RootCerts, TlsConfig};
use ureq::unversioned::multipart::{Form, Part};

const DEVICE_ID: &str = "piBooth";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// A q95 4:4:4 1.5 MP JPEG is ~0.3–1 MB; generous for slow Wi-Fi.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// Why a call failed, and whether trying again later can help.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// Network trouble, timeouts, 408/429 and 5xx: retry with backoff.
    Retry(String),
    /// 401/403: the key is wrong or lacks permission; every photo would fail.
    Auth(String),
    /// Unknown album, rejected file, redirect: needs a human; retried rarely.
    Permanent(String),
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Failure::Retry(m) => write!(f, "temporary: {m}"),
            Failure::Auth(m) | Failure::Permanent(m) => write!(f, "needs attention: {m}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Uploaded {
    pub id: String,
    /// The server already had this photo.
    pub duplicate: bool,
}

pub struct Immich {
    agent: Agent,
    /// e.g. `https://immich.example/api` (no trailing slash).
    base: String,
    key: String,
    album: Option<String>,
}

#[derive(Deserialize)]
struct UploadResponse {
    id: String,
    status: String,
}

#[derive(Deserialize)]
struct BulkIdResponse {
    success: bool,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Deserialize)]
struct Me {
    #[serde(default)]
    name: String,
    #[serde(default)]
    email: String,
}

/// HTTP status → failure kind.
pub fn classify(status: u16, body: &str) -> Failure {
    let msg = format!("HTTP {status}: {}", body.chars().take(200).collect::<String>().trim());
    match status {
        401 | 403 => Failure::Auth(msg),
        408 | 429 | 500..=599 => Failure::Retry(msg),
        _ => Failure::Permanent(msg),
    }
}

impl Immich {
    /// `base_url` is the API root as in v1's `.env` (`…/api`).
    pub fn new(base_url: &str, key: &str, album: Option<&str>) -> Self {
        let agent = Agent::config_builder()
            .timeout_connect(Some(CONNECT_TIMEOUT))
            .timeout_global(Some(REQUEST_TIMEOUT))
            .http_status_as_error(false)
            // A redirect means a wrong base_url (http vs https, missing /api).
            .max_redirects(0)
            // The Pi's certificate store: works with a private CA too.
            .tls_config(TlsConfig::builder().root_certs(RootCerts::PlatformVerifier).build())
            .build()
            .into();
        Self {
            agent,
            base: base_url.trim_end_matches('/').to_string(),
            key: key.to_string(),
            album: album.filter(|a| !a.is_empty()).map(str::to_string),
        }
    }

    pub fn album(&self) -> Option<&str> {
        self.album.as_deref()
    }

    /// Check the key: returns "name <email>" of the key's owner.
    pub fn whoami(&self) -> Result<String, Failure> {
        let mut resp = self
            .agent
            .get(format!("{}/users/me", self.base))
            .header("x-api-key", &self.key)
            .header("accept", "application/json")
            .call()
            .map_err(transport)?;
        let status = resp.status().as_u16();
        if status != 200 {
            return Err(failed(&mut resp));
        }
        let me: Me = resp.body_mut().read_json().map_err(|e| Failure::Retry(format!("bad response: {e}")))?;
        Ok(format!("{} <{}>", me.name, me.email))
    }

    /// Upload one JPEG. `taken` is RFC 3339 (fileCreatedAt/fileModifiedAt).
    pub fn upload(&self, photo: &Path, taken: &str) -> Result<Uploaded, Failure> {
        let name = photo.file_name().and_then(|n| n.to_str()).unwrap_or("photo.jpg");
        let part = Part::file(photo)
            .map_err(|e| Failure::Permanent(format!("cannot read {}: {e}", photo.display())))?
            .file_name(name)
            .mime_str("image/jpeg")
            .map_err(|e| Failure::Permanent(format!("mime: {e}")))?;
        let form = Form::new()
            .text("deviceAssetId", name)
            .text("deviceId", DEVICE_ID)
            .text("fileCreatedAt", taken)
            .text("fileModifiedAt", taken)
            .text("filename", name)
            .text("isFavorite", "false")
            .part("assetData", part);
        let mut resp = self
            .agent
            .post(format!("{}/assets", self.base))
            .header("x-api-key", &self.key)
            .header("accept", "application/json")
            .send(form)
            .map_err(transport)?;
        let status = resp.status().as_u16();
        if status != 200 && status != 201 {
            return Err(failed(&mut resp));
        }
        let r: UploadResponse =
            resp.body_mut().read_json().map_err(|e| Failure::Retry(format!("bad upload response: {e}")))?;
        Ok(Uploaded { id: r.id, duplicate: r.status == "duplicate" })
    }

    /// Add an uploaded asset to the configured album (no-op without one).
    /// An asset already in the album counts as success.
    pub fn add_to_album(&self, asset_id: &str) -> Result<(), Failure> {
        let Some(album) = &self.album else { return Ok(()) };
        let mut resp = self
            .agent
            .put(format!("{}/albums/{album}/assets", self.base))
            .header("x-api-key", &self.key)
            .header("accept", "application/json")
            .send_json(serde_json::json!({ "ids": [asset_id] }))
            .map_err(transport)?;
        let status = resp.status().as_u16();
        if status != 200 {
            return Err(failed(&mut resp));
        }
        let results: Vec<BulkIdResponse> =
            resp.body_mut().read_json().map_err(|e| Failure::Retry(format!("bad album response: {e}")))?;
        match results.first() {
            Some(r) if r.success || r.error.as_deref() == Some("duplicate") => Ok(()),
            Some(r) => Err(Failure::Permanent(format!(
                "album {album} refused asset: {}",
                r.error.as_deref().unwrap_or("unknown")
            ))),
            None => Err(Failure::Retry("empty album response".into())),
        }
    }
}

/// Failure for a non-success response, with 3xx reported as a config error.
fn failed(resp: &mut ureq::http::Response<ureq::Body>) -> Failure {
    let status = resp.status().as_u16();
    if (300..400).contains(&status) {
        let location = resp.headers().get("location").and_then(|v| v.to_str().ok()).unwrap_or("?");
        return Failure::Permanent(format!("server redirected to {location}: check immich.base_url"));
    }
    classify(status, &resp.body_mut().read_to_string().unwrap_or_default())
}

fn transport(e: ureq::Error) -> Failure {
    Failure::Retry(format!("network: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use mockito::Matcher;

    const ID: &str = "4c1d8f6e-0b2a-4f6e-9a7c-2d4e6f8a0b1c";

    fn photo() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("booth_20261119-185907.jpg");
        std::fs::write(&p, b"\xFF\xD8fake-jpeg\xFF\xD9").unwrap();
        (dir, p)
    }

    /// A URL on a port nothing listens on.
    fn dead_url() -> String {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://127.0.0.1:{}/api", l.local_addr().unwrap().port())
    }

    #[test]
    fn redirects_are_configuration_errors() {
        let mut server = mockito::Server::new();
        server
            .mock("POST", "/api/assets")
            .with_status(301)
            .with_header("location", "https://immich.example/api/assets")
            .create();
        server
            .mock("GET", "/api/users/me")
            .with_status(301)
            .with_header("location", "https://immich.example/api/users/me")
            .create();
        server
            .mock("PUT", "/api/albums/alb/assets")
            .with_status(301)
            .with_header("location", "https://immich.example/x")
            .create();
        let (_d, p) = photo();
        let c = Immich::new(&format!("{}/api", server.url()), "k", Some("alb"));
        for r in [c.upload(&p, "t").map(|_| ()), c.whoami().map(|_| ()), c.add_to_album(ID)] {
            assert!(
                matches!(&r, Err(Failure::Permanent(m)) if m.contains("redirected to https://immich.example") && m.contains("immich.base_url")),
                "{r:?}"
            );
        }
    }

    #[test]
    fn upload_sends_v1_compatible_form_and_parses_created() {
        let mut server = mockito::Server::new();
        let m = server
            .mock("POST", "/api/assets")
            .match_header("x-api-key", "secret")
            .match_header("content-type", Matcher::Regex("multipart/form-data; boundary=".into()))
            .match_body(Matcher::AllOf(vec![
                Matcher::Regex(r#"name="deviceAssetId"\r\n\r\nbooth_20261119-185907.jpg"#.into()),
                Matcher::Regex(r#"name="deviceId"\r\n\r\npiBooth"#.into()),
                Matcher::Regex(r#"name="fileCreatedAt"\r\n\r\n2026-11-19T17:59:07.818Z"#.into()),
                Matcher::Regex(r#"name="fileModifiedAt"\r\n\r\n2026-11-19T17:59:07.818Z"#.into()),
                Matcher::Regex(r#"name="assetData"; filename="booth_20261119-185907.jpg""#.into()),
                Matcher::Regex("fake-jpeg".into()),
            ]))
            .with_status(201)
            .with_header("content-type", "application/json")
            .with_body(format!(r#"{{"id":"{ID}","status":"created"}}"#))
            .create();
        let (_d, p) = photo();
        let c = Immich::new(&format!("{}/api/", server.url()), "secret", None);
        assert_eq!(c.upload(&p, "2026-11-19T17:59:07.818Z"), Ok(Uploaded { id: ID.into(), duplicate: false }));
        m.assert();
    }

    #[test]
    fn duplicate_is_success() {
        let mut server = mockito::Server::new();
        server
            .mock("POST", "/api/assets")
            .with_status(200)
            .with_body(format!(r#"{{"id":"{ID}","status":"duplicate"}}"#))
            .create();
        let (_d, p) = photo();
        let c = Immich::new(&format!("{}/api", server.url()), "k", None);
        assert!(c.upload(&p, "t").unwrap().duplicate);
    }

    #[test]
    fn errors_are_classified() {
        let mut server = mockito::Server::new();
        server.mock("POST", "/api/assets").with_status(401).with_body("Invalid API key").create();
        let (_d, p) = photo();
        let c = Immich::new(&format!("{}/api", server.url()), "bad", None);
        assert!(
            matches!(c.upload(&p, "t"), Err(Failure::Auth(m)) if m.contains("401") && m.contains("Invalid API key"))
        );
        assert!(matches!(classify(503, ""), Failure::Retry(_)));
        assert!(matches!(classify(429, ""), Failure::Retry(_)));
        assert!(matches!(classify(403, ""), Failure::Auth(_)));
        assert!(matches!(classify(408, ""), Failure::Retry(_)));
        assert!(matches!(classify(404, ""), Failure::Permanent(_)));
        assert!(matches!(classify(400, ""), Failure::Permanent(_)));
        // Nothing listening: a network failure, retried.
        let dead = Immich::new(&dead_url(), "k", None);
        assert!(matches!(dead.upload(&p, "t"), Err(Failure::Retry(_))));
        // Missing file: no point retrying.
        assert!(matches!(c.upload(Path::new("/nonexistent.jpg"), "t"), Err(Failure::Permanent(_))));
    }

    #[test]
    fn album_add_success_duplicate_and_refusal() {
        let mut server = mockito::Server::new();
        let ok = server
            .mock("PUT", "/api/albums/alb-1/assets")
            .match_header("x-api-key", "k")
            .match_body(Matcher::Json(serde_json::json!({ "ids": [ID] })))
            .with_status(200)
            .with_body(format!(r#"[{{"id":"{ID}","success":true}}]"#))
            .expect(1)
            .create();
        let c = Immich::new(&format!("{}/api", server.url()), "k", Some("alb-1"));
        assert_eq!(c.add_to_album(ID), Ok(()));
        ok.assert();

        server
            .mock("PUT", "/api/albums/alb-2/assets")
            .with_status(200)
            .with_body(format!(r#"[{{"id":"{ID}","success":false,"error":"duplicate"}}]"#))
            .create();
        assert_eq!(Immich::new(&format!("{}/api", server.url()), "k", Some("alb-2")).add_to_album(ID), Ok(()));

        server
            .mock("PUT", "/api/albums/alb-3/assets")
            .with_status(200)
            .with_body(format!(r#"[{{"id":"{ID}","success":false,"error":"no_permission"}}]"#))
            .create();
        let r = Immich::new(&format!("{}/api", server.url()), "k", Some("alb-3")).add_to_album(ID);
        assert!(matches!(r, Err(Failure::Permanent(m)) if m.contains("no_permission")));

        assert_eq!(Immich::new(&dead_url(), "k", None).add_to_album(ID), Ok(()), "no album: no call");
    }

    #[test]
    fn whoami_reports_owner_or_bad_key() {
        let mut server = mockito::Server::new();
        server
            .mock("GET", "/api/users/me")
            .match_header("x-api-key", "good")
            .with_status(200)
            .with_body(r#"{"id":"u","name":"Booth","email":"booth@example.com","isAdmin":false}"#)
            .create();
        server.mock("GET", "/api/users/me").match_header("x-api-key", "bad").with_status(401).create();
        let base = format!("{}/api", server.url());
        assert_eq!(Immich::new(&base, "good", None).whoami(), Ok("Booth <booth@example.com>".into()));
        assert!(matches!(Immich::new(&base, "bad", None).whoami(), Err(Failure::Auth(_))));
    }
}
