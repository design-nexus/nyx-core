//! Nextcloud WebDAV. Files live under `/remote.php/dav/files/<user>/<folder>/`.
//! Batch bytes are immutable. The manifest write is conditional on the ETag from
//! the last read. HTTP is refused except a loopback address when a test asks for it.

use crate::credentials::Secret;
use crate::crypto::{self, decrypt_batch, encrypt_batch};
use crate::provider::{Batch, BatchId, Etag, LoadedManifest, Manifest, PreconditionFailed, Remote};
use anyhow::Context;
use reqwest::blocking::Client;
use reqwest::redirect::Policy;
use std::time::Duration;
use url::Url;

pub struct WebDavConfig {
    pub server: String,
    pub username: String,
    pub password: Secret,
    pub folder: String,
    pub allow_insecure_loopback: bool,
    /// Age recipient. When set, batch bodies are encrypted. The manifest is not.
    pub recipient: Option<String>,
    /// Age identity used to read batches this device encrypted, or that another device encrypted for us.
    pub identity: Option<String>,
}

pub struct WebDav {
    client: Client,
    root: Url,
    username: String,
    password: Secret,
    recipient: Option<String>,
    identity: Option<String>,
}

impl WebDav {
    pub fn new(config: WebDavConfig) -> anyhow::Result<Self> {
        let root = collection_root(&config.server, &config.username, &config.folder, config.allow_insecure_loopback)?;
        let client = Client::builder()
            .redirect(Policy::custom(|attempt| {
                if attempt.previous().len() >= 5 {
                    return attempt.error("too many redirects");
                }
                let Some(prev) = attempt.previous().last() else { return attempt.follow() };
                let next = attempt.url();
                if prev.scheme() != next.scheme() || prev.host() != next.host() || prev.port() != next.port() {
                    return attempt.error("the server redirected to another host");
                }
                attempt.follow()
            }))
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(20))
            .build()?;
        Ok(Self {
            client,
            root,
            username: config.username,
            password: config.password,
            recipient: config.recipient,
            identity: config.identity,
        })
    }

    fn url(&self, name: &str) -> anyhow::Result<Url> {
        self.root.join(name).context("the sync folder is not a usable URL")
    }

    fn auth(&self, builder: reqwest::blocking::RequestBuilder) -> reqwest::blocking::RequestBuilder {
        builder.basic_auth(&self.username, Some(self.password.expose()))
    }

    fn status_error(status: reqwest::StatusCode) -> anyhow::Error {
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            anyhow::anyhow!("Nextcloud refused the username or app password")
        } else if status == reqwest::StatusCode::PRECONDITION_FAILED {
            anyhow::Error::new(PreconditionFailed)
        } else {
            anyhow::anyhow!("Nextcloud returned {status}")
        }
    }

    fn protect(&self, body: Vec<u8>) -> anyhow::Result<Vec<u8>> {
        match &self.recipient {
            Some(recipient) => encrypt_batch(&body, recipient),
            None => Ok(body),
        }
    }

    fn reveal(&self, body: Vec<u8>) -> anyhow::Result<Vec<u8>> {
        if !crypto::is_encrypted(&body) {
            return Ok(body);
        }
        let Some(identity) = &self.identity else {
            anyhow::bail!("a batch is encrypted and this device has no sync key");
        };
        decrypt_batch(&body, identity)
    }
}

impl Remote for WebDav {
    fn fetch_manifest(&self) -> anyhow::Result<LoadedManifest> {
        let response = self.auth(self.client.get(self.url("manifest.json")?)).send()?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(LoadedManifest { manifest: None, etag: None });
        }
        if !response.status().is_success() {
            return Err(Self::status_error(response.status()));
        }
        let etag = response.headers().get("etag").and_then(|v| v.to_str().ok()).map(|v| Etag(v.to_string()));
        let bytes = response.bytes()?;
        let manifest = serde_json::from_slice(&bytes)?;
        Ok(LoadedManifest { manifest: Some(manifest), etag })
    }

    fn write_manifest(&self, manifest: &Manifest, if_match: Option<&Etag>) -> anyhow::Result<Etag> {
        let _ = self.ensure_folder();
        let mut request = self.auth(self.client.put(self.url("manifest.json")?)).body(serde_json::to_vec(manifest)?);
        request = match if_match {
            Some(etag) => request.header("If-Match", &etag.0),
            None => request.header("If-None-Match", "*"),
        };
        let response = request.send()?;
        if !response.status().is_success() {
            return Err(Self::status_error(response.status()));
        }
        let etag =
            response.headers().get("etag").and_then(|v| v.to_str().ok()).map(|v| v.to_string()).unwrap_or_else(|| "\"0\"".into());
        Ok(Etag(etag))
    }

    fn fetch_batch(&self, id: &BatchId) -> anyhow::Result<Batch> {
        let response = self.auth(self.client.get(self.url(&format!("batches/{}.json", id.0))?)).send()?;
        if !response.status().is_success() {
            return Err(Self::status_error(response.status()));
        }
        let bytes = self.reveal(response.bytes()?.to_vec())?;
        #[derive(serde::Deserialize)]
        struct Head {
            schema: u32,
            id: String,
            device_id: String,
            base_generation: u64,
        }
        let head: Head = serde_json::from_slice(&bytes)?;
        Ok(Batch {
            schema: head.schema,
            id: BatchId(head.id),
            device_id: head.device_id,
            base_generation: head.base_generation,
            body: bytes,
        })
    }

    fn write_batch(&self, batch: &Batch) -> anyhow::Result<()> {
        let _ = self.ensure_folder();
        let body = self.protect(batch.body.clone())?;
        let response = self
            .auth(self.client.put(self.url(&format!("batches/{}.json", batch.id.0))?))
            .header("If-None-Match", "*")
            .body(body)
            .send()?;
        if response.status().is_success() || response.status() == reqwest::StatusCode::NO_CONTENT {
            return Ok(());
        }
        Err(Self::status_error(response.status()))
    }
}

impl WebDav {
    fn ensure_folder(&self) -> anyhow::Result<()> {
        let response =
            self.auth(self.client.request(reqwest::Method::from_bytes(b"MKCOL").unwrap(), self.root.clone())).send()?;
        if response.status().is_success()
            || response.status() == reqwest::StatusCode::METHOD_NOT_ALLOWED
            || response.status() == reqwest::StatusCode::CONFLICT
            || response.status() == reqwest::StatusCode::NOT_FOUND
        {
            return Ok(());
        }
        Err(Self::status_error(response.status()))
    }
}

impl crate::provider::SyncProvider for WebDav {
    async fn load_manifest(&self) -> anyhow::Result<Option<Manifest>> {
        Ok(Remote::fetch_manifest(self)?.manifest)
    }

    async fn store_manifest(&self, manifest: &Manifest, if_match: Option<&Etag>) -> anyhow::Result<Etag> {
        Remote::write_manifest(self, manifest, if_match)
    }

    async fn get_batch(&self, id: &BatchId) -> anyhow::Result<Batch> {
        Remote::fetch_batch(self, id)
    }

    async fn put_batch(&self, batch: &Batch) -> anyhow::Result<()> {
        Remote::write_batch(self, batch)
    }
}

fn collection_root(server: &str, username: &str, folder: &str, allow_insecure_loopback: bool) -> anyhow::Result<Url> {
    let mut url = Url::parse(server.trim()).context("the Nextcloud address is not a URL")?;
    let loopback = matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "::1"));
    match url.scheme() {
        "https" => {}
        "http" if allow_insecure_loopback && loopback => {}
        _ => anyhow::bail!("Nextcloud sync requires https"),
    }
    anyhow::ensure!(url.username().is_empty() && url.password().is_none(), "put the username in the account field, not the URL");
    let folder = folder.trim().trim_matches('/');
    anyhow::ensure!(!username.is_empty() && !folder.is_empty(), "the account needs a username and a folder");
    let user = encode_segment(username);
    let folder = folder.split('/').filter(|part| !part.is_empty()).map(encode_segment).collect::<Vec<_>>().join("/");
    url.set_path(&format!("/remote.php/dav/files/{user}/{folder}/"));
    Ok(url)
}

fn encode_segment(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        let plain = byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~');
        if plain {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

#[cfg(any(test, feature = "testutil"))]
mod stub_server {
    use super::*;
    use std::collections::HashMap;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread;

    struct File {
        etag: String,
        body: Vec<u8>,
    }

    #[derive(Clone)]
    pub struct StubServer {
        url: String,
        files: Arc<Mutex<HashMap<String, File>>>,
        user: String,
        password: String,
        puts: Arc<AtomicUsize>,
        fail_manifest: Arc<AtomicUsize>,
    }

    impl StubServer {
        pub fn start(user: &str, password: &str) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let files = Arc::new(Mutex::new(HashMap::new()));
            let puts = Arc::new(AtomicUsize::new(0));
            let fail_manifest = Arc::new(AtomicUsize::new(0));
            let shared = files.clone();
            let puts_bg = puts.clone();
            let fail_bg = fail_manifest.clone();
            let user_bg = user.to_string();
            let pass_bg = password.to_string();
            thread::spawn(move || serve(listener, shared, user_bg, pass_bg, puts_bg, fail_bg));
            Self {
                url: format!("http://127.0.0.1:{port}"),
                files,
                user: user.into(),
                password: password.into(),
                puts,
                fail_manifest,
            }
        }

        pub fn url(&self) -> &str {
            &self.url
        }

        pub fn client(&self) -> anyhow::Result<WebDav> {
            WebDav::new(WebDavConfig {
                server: self.url.clone(),
                username: self.user.clone(),
                password: Secret::new(self.password.clone())?,
                folder: "Nyx/News".into(),
                allow_insecure_loopback: true,
                recipient: None,
                identity: None,
            })
        }

        pub fn puts(&self) -> usize {
            self.puts.load(Ordering::SeqCst)
        }

        pub fn fail_next_manifests(&self, n: usize) {
            self.fail_manifest.store(n, Ordering::SeqCst);
        }

        pub fn file(&self, name: &str) -> Option<Vec<u8>> {
            self.files.lock().unwrap().get(name).map(|file| file.body.clone())
        }
    }

    fn serve(
        listener: TcpListener,
        files: Arc<Mutex<HashMap<String, File>>>,
        user: String,
        password: String,
        puts: Arc<AtomicUsize>,
        fail_manifest: Arc<AtomicUsize>,
    ) {
        for conn in listener.incoming() {
            let Ok(mut conn) = conn else { continue };
            let _ = conn.set_read_timeout(Some(Duration::from_secs(2)));
            let mut buf = Vec::new();
            let mut tmp = [0; 2048];
            let request = loop {
                match conn.read(&mut tmp) {
                    Ok(0) | Err(_) => break None,
                    Ok(n) => {
                        buf.extend_from_slice(&tmp[..n]);
                        let Some(header_end) = find_header(&buf) else { continue };
                        let length = content_length(&String::from_utf8_lossy(&buf[..header_end]));
                        while buf.len() < header_end + 4 + length {
                            match conn.read(&mut tmp) {
                                Ok(0) | Err(_) => break,
                                Ok(n) => buf.extend_from_slice(&tmp[..n]),
                            }
                        }
                        let header = String::from_utf8_lossy(&buf[..header_end]).to_string();
                        let body = buf.get(header_end + 4..header_end + 4 + length).unwrap_or(&[]).to_vec();
                        break Some((header, body));
                    }
                }
            };
            if let Some((header, body)) = request {
                let _ = conn.write_all(&reply(&header, &body, &files, &user, &password, &puts, &fail_manifest));
            }
        }
    }

    fn find_header(buf: &[u8]) -> Option<usize> {
        buf.windows(4).position(|w| w == b"\r\n\r\n")
    }

    fn content_length(header: &str) -> usize {
        header
            .lines()
            .find_map(|line| {
                line.split_once(':')
                    .and_then(|(name, value)| (name.eq_ignore_ascii_case("content-length")).then(|| value.trim().parse().ok()))
            })
            .flatten()
            .unwrap_or(0)
    }

    fn header_value<'a>(header: &'a str, name: &str) -> Option<&'a str> {
        header.lines().find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.eq_ignore_ascii_case(name).then(|| value.trim())
        })
    }

    fn reply(
        header: &str,
        body: &[u8],
        files: &Mutex<HashMap<String, File>>,
        user: &str,
        password: &str,
        puts: &AtomicUsize,
        fail_manifest: &AtomicUsize,
    ) -> Vec<u8> {
        let mut lines = header.lines();
        let Some(request) = lines.next() else { return http(400, None, b"") };
        let mut parts = request.split(' ');
        let method = parts.next().unwrap_or("");
        let path = parts.next().unwrap_or("/");
        let expected = format!("Basic {}", b64(&format!("{user}:{password}")));
        if header_value(header, "authorization") != Some(expected.as_str()) {
            return http(401, None, b"");
        }
        let owned;
        let key: &str = if path.ends_with("/manifest.json") {
            "manifest.json"
        } else if path.contains("/batches/") {
            owned = format!("batches/{}", path.rsplit('/').next().unwrap_or(""));
            &owned
        } else {
            ""
        };
        if method == "MKCOL" || method == "OPTIONS" {
            return http(201, None, b"");
        }
        if key.is_empty() {
            return http(404, None, b"");
        }
        let mut files = files.lock().unwrap();
        if method == "GET" {
            let Some(file) = files.get(key) else { return http(404, None, b"") };
            return http(200, Some(&file.etag), &file.body);
        }
        if method == "PUT" {
            let if_match = header_value(header, "if-match");
            let if_none = header_value(header, "if-none-match");
            if key == "manifest.json" && fail_manifest.load(Ordering::SeqCst) > 0 {
                fail_manifest.fetch_sub(1, Ordering::SeqCst);
                return http(412, None, b"");
            }
            if if_none == Some("*") && files.contains_key(key) {
                return http(412, None, b"");
            }
            if let Some(tag) = if_match {
                match files.get(key) {
                    Some(file) if file.etag == tag => {}
                    _ => return http(412, None, b""),
                }
            }
            let etag = format!("\"{}\"", files.len() + body.len() + 1);
            files.insert(key.to_string(), File { etag: etag.clone(), body: body.to_vec() });
            puts.fetch_add(1, Ordering::SeqCst);
            return http(201, Some(&etag), b"");
        }
        http(405, None, b"")
    }

    fn http(status: u16, etag: Option<&str>, body: &[u8]) -> Vec<u8> {
        let reason = match status {
            200 => "OK",
            201 => "Created",
            401 => "Unauthorized",
            404 => "Not Found",
            405 => "Method Not Allowed",
            412 => "Precondition Failed",
            _ => "Error",
        };
        let mut head = format!("HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n", body.len());
        if let Some(etag) = etag {
            head.push_str(&format!("ETag: {etag}\r\n"));
        }
        head.push_str("\r\n");
        let mut out = head.into_bytes();
        out.extend_from_slice(body);
        out
    }

    fn b64(value: &str) -> String {
        const TABLE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let bytes = value.as_bytes();
        let mut out = String::new();
        for chunk in bytes.chunks(3) {
            let mut n = (chunk[0] as u32) << 16;
            if chunk.len() > 1 {
                n |= (chunk[1] as u32) << 8;
            }
            if chunk.len() > 2 {
                n |= chunk[2] as u32;
            }
            out.push(TABLE[((n >> 18) & 63) as usize] as char);
            out.push(TABLE[((n >> 12) & 63) as usize] as char);
            out.push(if chunk.len() > 1 { TABLE[((n >> 6) & 63) as usize] as char } else { '=' });
            out.push(if chunk.len() > 2 { TABLE[(n & 63) as usize] as char } else { '=' });
        }
        out
    }
}

#[cfg(any(test, feature = "testutil"))]
pub use stub_server::StubServer;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exchange::{encode_batch, sync_exchange};
    use crate::merge::OpKind;

    fn op(device: &str) -> crate::merge::Op {
        crate::merge::Op {
            hlc: crate::hlc::Hlc { wall_ms: 1, counter: 0, device: device.into() },
            collection: "feeds".into(),
            record_id: "1".into(),
            kind: OpKind::Upsert,
            fields: [("url".into(), "https://example.com/feed".into())].into(),
            member: None,
        }
    }

    #[test]
    fn http_to_a_public_host_is_rejected() {
        let built = WebDav::new(WebDavConfig {
            server: "http://cloud.example".into(),
            username: "ken".into(),
            password: Secret::new("secret").unwrap(),
            folder: "Nyx/News".into(),
            allow_insecure_loopback: true,
            recipient: None,
            identity: None,
        });
        let Err(err) = built else { panic!("http host was accepted") };
        assert!(err.to_string().contains("https"), "{err}");
    }

    #[test]
    fn a_wrong_password_writes_nothing() {
        let stub = StubServer::start("ken", "secret");
        let client = WebDav::new(WebDavConfig {
            server: stub.url().into(),
            username: "ken".into(),
            password: Secret::new("nope").unwrap(),
            folder: "Nyx/News".into(),
            allow_insecure_loopback: true,
            recipient: None,
            identity: None,
        })
        .unwrap();
        let err = client.fetch_manifest().unwrap_err();
        assert!(err.to_string().contains("refused"), "{err}");
        assert_eq!(stub.puts(), 0);
        assert!(stub.file("manifest.json").is_none());
    }

    #[test]
    fn a_lost_manifest_race_is_retried() {
        let stub = StubServer::start("ken", "secret");
        let client = stub.client().unwrap();
        let first = encode_batch("device-a", 1, 0, &[op("device-a")]).unwrap();
        sync_exchange(&client, "device-a", 0, std::slice::from_ref(&op("device-a"))).unwrap();
        assert!(stub.file("batches/1.json").is_some());
        stub.fail_next_manifests(1);
        let again = sync_exchange(&client, "device-a", 1, &[op("device-a")]).unwrap();
        assert!(again.pushed);
        assert_eq!(again.generation, 2);
        let _ = first;
    }

    #[test]
    fn encrypted_batches_hide_the_feed_url() {
        let (identity, recipient) = crypto::generate_identity();
        let stub = StubServer::start("ken", "secret");
        let client = WebDav::new(WebDavConfig {
            server: stub.url().into(),
            username: "ken".into(),
            password: Secret::new("secret").unwrap(),
            folder: "Nyx/News".into(),
            allow_insecure_loopback: true,
            recipient: Some(recipient),
            identity: Some(identity),
        })
        .unwrap();
        sync_exchange(&client, "device-a", 0, &[op("device-a")]).unwrap();
        let stored = stub.file("batches/1.json").unwrap();
        let needle = b"example.com";
        assert!(crypto::is_encrypted(&stored));
        assert!(!stored.windows(needle.len()).any(|w| w == needle));
        let loaded = client.fetch_batch(&BatchId("1".into())).unwrap();
        assert!(loaded.body.windows(needle.len()).any(|w| w == needle));
    }
}
