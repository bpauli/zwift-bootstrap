//! The fake CDN and the install assertions, shared by the test binaries.
//!
//! The fake CDN builds `Zwift_ver_cur.xml` and the manifest from a list of
//! `(path, bytes)` with correct CRCs (a test-only XML writer), serves them with
//! tiny_http, can inject faults per URL path, and logs every request path.

// Each test binary uses a different subset of these helpers.
#![allow(dead_code)]

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::{self, Read, Write};
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use zwift_bootstrap::{Bootstrapper, Config, Error, Event, SyncMode, SyncOutcome};

pub const ROOT: &str = "/root/";
pub const MODIFIED: &str = "2026-10-01 17:32:38.235";
pub const MODIFIED_UNIX_MILLIS: u64 = 1_790_875_958_235;

pub struct FakeCdn {
    pub base_url: String,
    pub requests: Arc<Mutex<Vec<String>>>,
    faults: Arc<Mutex<HashMap<String, Fault>>>,
    published: Arc<Mutex<Published>>,
    server: Arc<tiny_http::Server>,
}

#[derive(Clone)]
pub enum Fault {
    Status(u16),
    /// Serve flipped bytes for the first `n` requests, then the real body.
    CorruptTimes(u32),
    /// Serve `n` extra bytes past the manifest length, once.
    TooLong(usize),
    /// Gzip the body with `Content-Encoding: gzip` (the manifest, like CloudFront).
    Gzip,
    /// Sleep between chunks, so a test can kill the process mid-file.
    Slow,
}

pub struct Release<'a> {
    pub version: &'a str,
    pub files: &'a [(&'a str, &'a [u8])], // manifest path with `\`, content
}

/// What one release looks like on the CDN, and so in a correct install.
#[derive(Clone)]
pub struct Published {
    pub ver_cur: Vec<u8>,
    pub manifest_name: String,
    pub manifest: Vec<u8>,
    /// Install-relative path with `/`, content.
    pub files: Vec<(String, Vec<u8>)>,
    /// Decoded URL path, body.
    bodies: HashMap<String, Vec<u8>>,
}

pub fn crc(bytes: &[u8]) -> i32 {
    crc32fast::hash(bytes) as i32
}

pub fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

impl Published {
    fn build(release: &Release<'_>) -> Published {
        let mut hasher = crc32fast::Hasher::new();
        for (path, bytes) in release.files {
            hasher.update(path.as_bytes());
            hasher.update(bytes);
        }
        let folder = format!("Zwift_{}_{:08x}", release.version, hasher.finalize());
        let manifest_name = format!("{folder}_manifest.xml");
        let mut manifest = format!(
            "<Zwift version=\"{}\" sversion=\"test\" folder=\"{folder}\">\r\n    <files>\r\n",
            release.version
        );
        for (path, bytes) in release.files {
            manifest += &format!(
                "        <file>\r\n            <path>{}</path>\r\n            <length>{}</length>\r\n            <modified>{MODIFIED}</modified>\r\n            <checksum>{}</checksum>\r\n        </file>\r\n",
                xml_escape(path),
                bytes.len(),
                crc(bytes)
            );
        }
        manifest += "    </files>\r\n</Zwift>\r\n";
        let manifest = manifest.into_bytes();
        let unsigned = format!(
            "<Zwift version=\"{}\" sversion=\"test\" manifest=\"{manifest_name}\" manifest_checksum=\"{}\" ver_cur_checksum=\"-1\"/>\r\n",
            release.version,
            crc(&manifest)
        );
        let ver_cur = unsigned
            .replace("ver_cur_checksum=\"-1\"", &format!("ver_cur_checksum=\"{}\"", crc(unsigned.as_bytes())))
            .into_bytes();
        let files: Vec<(String, Vec<u8>)> =
            release.files.iter().map(|(p, b)| (p.replace('\\', "/"), b.to_vec())).collect();
        let mut bodies: HashMap<String, Vec<u8>> =
            files.iter().map(|(p, b)| (format!("{ROOT}{folder}/{p}"), b.clone())).collect();
        bodies.insert(format!("{ROOT}Zwift_ver_cur.xml"), ver_cur.clone());
        bodies.insert(format!("{ROOT}{manifest_name}"), manifest.clone());
        Published { ver_cur, manifest_name, manifest, files, bodies }
    }
}

pub struct SlowBody {
    data: Vec<u8>,
    pos: usize,
}

impl Read for SlowBody {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        thread::sleep(Duration::from_millis(20));
        let n = buf.len().min(1024).min(self.data.len() - self.pos);
        buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

impl FakeCdn {
    pub fn start(release: Release<'_>) -> FakeCdn {
        let server = Arc::new(tiny_http::Server::http("127.0.0.1:0").unwrap());
        let port = server.server_addr().to_ip().unwrap().port();
        let cdn = FakeCdn {
            base_url: format!("http://127.0.0.1:{port}{ROOT}"),
            requests: Arc::default(),
            faults: Arc::default(),
            published: Arc::new(Mutex::new(Published::build(&release))),
            server: Arc::clone(&server),
        };
        let (requests, faults, published) =
            (Arc::clone(&cdn.requests), Arc::clone(&cdn.faults), Arc::clone(&cdn.published));
        thread::spawn(move || {
            for request in server.incoming_requests() {
                let (requests, faults, published) =
                    (Arc::clone(&requests), Arc::clone(&faults), Arc::clone(&published));
                thread::spawn(move || respond(request, &requests, &faults, &published));
            }
        });
        cdn
    }
    pub fn publish(&self, release: Release<'_>) {
        *self.published.lock().unwrap() = Published::build(&release);
    }
    pub fn fault(&self, url_path: &str, fault: Fault) {
        self.faults.lock().unwrap().insert(url_path.to_owned(), fault);
    }
    pub fn clear_faults(&self) {
        self.faults.lock().unwrap().clear();
    }
    pub fn published(&self) -> Published {
        self.published.lock().unwrap().clone()
    }
    /// Drains the log, so each run's requests can be asserted on their own.
    pub fn take_requests(&self) -> Vec<String> {
        std::mem::take(&mut *self.requests.lock().unwrap())
    }
    /// Install-relative paths of the game files requested so far (drains the log).
    pub fn game_file_requests(&self) -> Vec<String> {
        let mut paths: Vec<String> = self
            .take_requests()
            .iter()
            .filter_map(|r| r.strip_prefix(ROOT)?.split_once('/').map(|(_, p)| p.to_owned()))
            .collect();
        paths.sort();
        paths
    }
}

impl Drop for FakeCdn {
    fn drop(&mut self) {
        self.server.unblock();
    }
}

pub fn respond(
    request: tiny_http::Request,
    requests: &Mutex<Vec<String>>,
    faults: &Mutex<HashMap<String, Fault>>,
    published: &Mutex<Published>,
) {
    let path = percent_encoding::percent_decode_str(request.url()).decode_utf8_lossy().into_owned();
    requests.lock().unwrap().push(path.clone());
    let Some(mut body) = published.lock().unwrap().bodies.get(&path).cloned() else {
        let _ = request.respond(tiny_http::Response::empty(404));
        return;
    };
    let fault = {
        let mut faults = faults.lock().unwrap();
        let key = faults.keys().find(|k| path.ends_with(k.as_str())).cloned();
        match key.as_ref().and_then(|k| faults.get_mut(k)) {
            Some(Fault::CorruptTimes(0)) | None => None,
            Some(Fault::CorruptTimes(n)) => {
                *n -= 1;
                Some(Fault::CorruptTimes(1))
            }
            Some(Fault::TooLong(_)) => key.and_then(|k| faults.remove(&k)),
            Some(other) => Some(other.clone()),
        }
    };
    let mut headers = Vec::new();
    match fault {
        Some(Fault::Status(code)) => {
            let _ = request.respond(tiny_http::Response::empty(code));
            return;
        }
        Some(Fault::CorruptTimes(_)) => body.iter_mut().for_each(|b| *b ^= 0x5a),
        Some(Fault::TooLong(n)) => body.extend(std::iter::repeat_n(b'!', n)),
        Some(Fault::Gzip) => {
            let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            encoder.write_all(&body).unwrap();
            body = encoder.finish().unwrap();
            headers.push(tiny_http::Header::from_bytes("Content-Encoding", "gzip").unwrap());
        }
        Some(Fault::Slow) => {
            let len = body.len();
            let reader = SlowBody { data: body, pos: 0 };
            let _ = request.respond(tiny_http::Response::new(200.into(), headers, reader, Some(len), None));
            return;
        }
        None => {}
    }
    let len = body.len();
    let _ = request.respond(tiny_http::Response::new(200.into(), headers, &body[..], Some(len), None));
}

pub struct Dirs {
    pub root: tempfile::TempDir,
    pub install: PathBuf,
    pub staging: PathBuf,
}

pub fn dirs() -> Dirs {
    let root = tempfile::tempdir().unwrap();
    let install = root.path().join("install");
    let staging = root.path().join("staging");
    Dirs { root, install, staging }
}

pub fn bootstrapper(cdn: &FakeCdn, dirs: &Dirs) -> Bootstrapper {
    Bootstrapper::new(Config {
        install_dir: dirs.install.clone(),
        staging_dir: dirs.staging.clone(),
        base_url: cdn.base_url.clone(),
        jobs: NonZeroUsize::new(4).unwrap(),
    })
    .unwrap()
}

pub fn run(cdn: &FakeCdn, dirs: &Dirs, mode: SyncMode) -> (Result<SyncOutcome, Error>, Vec<Event>) {
    let mut events = Vec::new();
    let result = bootstrapper(cdn, dirs).sync(mode, &mut |e| events.push(e));
    (result, events)
}

pub fn sync(cdn: &FakeCdn, dirs: &Dirs, mode: SyncMode) -> SyncOutcome {
    run(cdn, dirs, mode).0.unwrap()
}

/// Every regular file under `dir`, keyed by `/`-joined relative path.
pub fn tree(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        let Ok(entries) = fs::read_dir(dir) else { return };
        for entry in entries {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(base, &path, out);
            } else {
                let rel = path.strip_prefix(base).unwrap().to_string_lossy().into_owned();
                out.insert(rel, fs::read(&path).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

pub fn expected_install(release: &Published) -> BTreeMap<String, Vec<u8>> {
    let mut want: BTreeMap<String, Vec<u8>> = release.files.iter().cloned().collect();
    want.insert(".zwift-bootstrap/lock".into(), Vec::new());
    want.insert("Zwift_ver_cur.xml".into(), release.ver_cur.clone());
    want.insert(release.manifest_name.clone(), release.manifest.clone());
    want
}

/// The exact end state of a clean run: game files, the server ver_cur and manifest
/// verbatim, the lock, and nothing else (no marker, no old manifest, no extra file).
pub fn assert_installed(dirs: &Dirs, release: &Published) {
    assert_eq!(tree(&dirs.install), expected_install(release));
    assert!(tree(&dirs.staging.join("files")).is_empty(), "staging is cleared");
}

pub fn write(path: &Path, bytes: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

pub const A: Release = Release {
    version: "1.0.1",
    files: &[
        ("ZwiftApp.exe", b"app v1"),
        (r"data\Worlds\world9\map 04.ztx", b"map four"),
        (r"assets\x\y.bin", b"assets y"),
        (r"data\old\gone.bin", b"leaves in B"),
        ("unchanged.dll", b"same in every version"),
    ],
};

/// From A: changed app, removed `assets\x\y.bin` and `data\old\gone.bin`, new `assets\x`
/// (a file where A had a dir) and `data\new.bin`.
pub const B: Release = Release {
    version: "1.0.2",
    files: &[
        ("ZwiftApp.exe", b"app v2!"),
        (r"data\Worlds\world9\map 04.ztx", b"map four"),
        (r"assets\x", b"x is a file now"),
        (r"data\new.bin", b"new in B"),
        ("unchanged.dll", b"same in every version"),
    ],
};
