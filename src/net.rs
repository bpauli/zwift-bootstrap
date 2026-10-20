//! The CDN. One fetch primitive with verification and retries. ureq types stay here.

use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::Path;
use std::thread;
use std::time::Duration;

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};

use crate::Error;
use crate::model::{Checksum, Crc32, Manifest, ManifestName, RelPath, ServerFolder, VerCur};
use crate::parallel::Sink;
use crate::{disk, wire};

const ATTEMPTS: u32 = 4;
const VER_CUR_MAX: u64 = 64 * 1024;
const MANIFEST_MAX: u64 = 256 * 1024 * 1024;
/// The body timeout is `length / MIN_RATE`, at least `BODY_TIMEOUT_FLOOR`.
const MIN_RATE: u64 = 64 * 1024;
const BODY_TIMEOUT_FLOOR: Duration = Duration::from_secs(60);

/// RFC 3986 unreserved characters stay literal; everything else is encoded.
const SEGMENT: &AsciiSet = &NON_ALPHANUMERIC.remove(b'-').remove(b'.').remove(b'_').remove(b'~');

/// What a download must be, checked while streaming.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Expect {
    pub size: Size,
    pub checksum: Checksum,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Size {
    /// Game files. Abort as soon as the body passes this, fail if it ends short.
    Exact(u64),
    /// Control files of unknown length. A cap so a hostile server cannot fill the disk.
    AtMost(u64),
}

/// After retries, why one download did not produce a verified file.
#[derive(Debug)]
pub(crate) struct FetchFailure {
    pub url: String,
    pub cause: Cause,
}

#[derive(Debug)]
pub(crate) enum Cause {
    Mismatch { expected: Crc32, actual: Crc32 },
    Other(String),
}

impl fmt::Display for Cause {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Cause::Mismatch { expected, actual } => {
                write!(f, "checksum mismatch: expected {expected}, got {actual}")
            }
            Cause::Other(text) => f.write_str(text),
        }
    }
}

impl From<FetchFailure> for Error {
    fn from(failure: FetchFailure) -> Error {
        Error::Download { url: failure.url, cause: failure.cause.to_string() }
    }
}

/// One attempt's failure, classified for the retry loop.
enum Attempt {
    /// Network error, 5xx, 429, short body, long body, CRC mismatch.
    Transient(Cause),
    /// Other 4xx, local I/O error (disk full, permissions).
    Permanent(Cause),
}

fn transient(text: impl Into<String>) -> Attempt {
    Attempt::Transient(Cause::Other(text.into()))
}

fn permanent(text: impl Into<String>) -> Attempt {
    Attempt::Permanent(Cause::Other(text.into()))
}

pub(crate) struct Remote {
    agent: ureq::Agent,
    /// `http(s)://...`, always ends with `/`.
    base: String,
}

impl Remote {
    pub(crate) fn new(base_url: &str) -> Result<Remote, Error> {
        let has_host = |rest: &str| !rest.is_empty() && !rest.starts_with('/');
        let valid = base_url.strip_prefix("https://").or_else(|| base_url.strip_prefix("http://"));
        if !valid.is_some_and(has_host) {
            return Err(Error::BaseUrl(base_url.to_owned()));
        }
        let mut base = base_url.to_owned();
        if !base.ends_with('/') {
            base.push('/');
        }
        let agent = ureq::Agent::config_builder()
            .timeout_connect(Some(Duration::from_secs(15)))
            .timeout_recv_response(Some(Duration::from_secs(60)))
            .user_agent(concat!("zwift-bootstrap/", env!("CARGO_PKG_VERSION")))
            .build()
            .into();
        Ok(Remote { agent, base })
    }

    pub(crate) fn ver_cur_url(&self) -> String {
        format!("{}{}", self.base, disk::VER_CUR)
    }

    pub(crate) fn manifest_url(&self, name: &ManifestName) -> String {
        format!("{}{}", self.base, name.as_str())
    }

    /// `{base}{folder}/{path}` with each component percent-encoded (NON_ALPHANUMERIC
    /// minus `-._~`). Never `%5C`: the separator is a literal `/` (the CDN 403s on `%5C`).
    pub(crate) fn file_url(&self, folder: &ServerFolder, path: &RelPath) -> String {
        let mut url = format!("{}{}", self.base, folder.as_str());
        for component in path.components() {
            url.push('/');
            url.extend(utf8_percent_encode(component, SEGMENT));
        }
        url
    }

    /// Always fresh: the ver_cur is the question "is there an update", never cached.
    pub(crate) fn fetch_ver_cur(&self, dest: &Path, sink: &Sink) -> Result<VerCur, Error> {
        let url = self.ver_cur_url();
        let expect = Expect { size: Size::AtMost(VER_CUR_MAX), checksum: Checksum::Absent };
        self.fetch(&url, dest, expect, sink, &|_, _| {})?;
        let bytes = fs::read(dest).map_err(|source| Error::Io { path: dest.to_owned(), source })?;
        wire::parse_ver_cur(&bytes).map_err(|reason| Error::BadServerData { what: "ver_cur", reason })
    }

    /// Reuse `dest` when its CRC already equals `ver_cur.manifest_checksum` (a rerun after
    /// a crash skips 5 MB). Else download; a CRC mismatch after retries is
    /// `Error::ManifestChecksum`.
    pub(crate) fn fetch_manifest(&self, ver_cur: &VerCur, dest: &Path, sink: &Sink) -> Result<Manifest, Error> {
        let reusable = match ver_cur.manifest_checksum {
            Checksum::Crc(want) => disk::crc_file(dest).is_ok_and(|have| have == want),
            Checksum::Absent => false,
        };
        if !reusable {
            let url = self.manifest_url(&ver_cur.manifest);
            let expect = Expect { size: Size::AtMost(MANIFEST_MAX), checksum: ver_cur.manifest_checksum };
            self.fetch(&url, dest, expect, sink, &|_, _| {}).map_err(|failure| match failure.cause {
                Cause::Mismatch { expected, actual } => Error::ManifestChecksum { expected, actual },
                Cause::Other(_) => failure.into(),
            })?;
        }
        let bytes = fs::read(dest).map_err(|source| Error::Io { path: dest.to_owned(), source })?;
        let bad = |reason| Error::BadServerData { what: "manifest", reason };
        let manifest = wire::parse_manifest(&bytes).map_err(bad)?;
        if manifest.version() != ver_cur.version {
            return Err(bad(format!("version {} differs from the ver_cur {}", manifest.version(), ver_cur.version)));
        }
        Ok(manifest)
    }

    /// Download `url` to `dest` and leave `dest` only if it passes `expect`. Retries
    /// `Transient` failures with 1s/2s/4s backoff and reports each through `on_retry`.
    ///
    /// Streams: read 64 KiB -> crc32fast::Hasher::update -> write -> sink.bytes(+n).
    /// On a failed attempt: sink.bytes(-written), so progress never overcounts.
    pub(crate) fn fetch(
        &self,
        url: &str,
        dest: &Path,
        expect: Expect,
        sink: &Sink,
        on_retry: &dyn Fn(u32, &Cause),
    ) -> Result<(), FetchFailure> {
        let mut attempt = 1;
        loop {
            let mut written = 0;
            let result = self.attempt(url, dest, expect, sink, &mut written);
            if result.is_err() {
                sink.bytes(-(written as i64));
                let _ = fs::remove_file(dest);
            }
            match result {
                Ok(()) => return Ok(()),
                Err(Attempt::Transient(cause)) if attempt < ATTEMPTS => {
                    on_retry(attempt, &cause);
                    thread::sleep(Duration::from_secs(1 << (attempt - 1)));
                    attempt += 1;
                }
                Err(Attempt::Transient(cause) | Attempt::Permanent(cause)) => {
                    return Err(FetchFailure { url: url.to_owned(), cause });
                }
            }
        }
    }

    fn attempt(&self, url: &str, dest: &Path, expect: Expect, sink: &Sink, written: &mut u64) -> Result<(), Attempt> {
        let (cap, exact) = match expect.size {
            Size::Exact(len) => (len, Some(len)),
            Size::AtMost(max) => (max, None),
        };
        let body_timeout = BODY_TIMEOUT_FLOOR.max(Duration::from_secs(cap / MIN_RATE));
        let response = self
            .agent
            .get(url)
            .config()
            .timeout_recv_body(Some(body_timeout))
            .build()
            .call()
            .map_err(classify)?;
        let mut body = response.into_body().into_reader();
        let mut file = File::create(dest).map_err(|e| permanent(format!("{}: {e}", dest.display())))?;
        let mut hasher = crc32fast::Hasher::new();
        let mut buf = vec![0; 64 * 1024];
        loop {
            let n = match body.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(transient(format!("reading body: {e}"))),
            };
            if *written + n as u64 > cap {
                return Err(match exact {
                    Some(len) => transient(format!("body longer than {len} bytes")),
                    None => permanent(format!("body longer than the {cap} byte limit")),
                });
            }
            hasher.update(&buf[..n]);
            file.write_all(&buf[..n]).map_err(|e| permanent(format!("{}: {e}", dest.display())))?;
            *written += n as u64;
            sink.bytes(n as i64);
        }
        if let Some(len) = exact.filter(|&len| *written != len) {
            return Err(transient(format!("body ended after {written} of {len} bytes")));
        }
        if let Checksum::Crc(expected) = expect.checksum {
            let actual = Crc32(hasher.finalize());
            if actual != expected {
                return Err(Attempt::Transient(Cause::Mismatch { expected, actual }));
            }
        }
        Ok(())
    }
}

fn classify(error: ureq::Error) -> Attempt {
    match error {
        ureq::Error::StatusCode(code) if code == 429 || code >= 500 => transient(format!("HTTP {code}")),
        ureq::Error::StatusCode(code) => permanent(format!("HTTP {code}")),
        e @ (ureq::Error::BadUri(_) | ureq::Error::Http(_) | ureq::Error::InvalidProxyUrl) => {
            permanent(e.to_string())
        }
        e => transient(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_url_keeps_slash_separators_and_encodes_each_component() {
        let remote = Remote::new("https://cdn.example/root").unwrap();
        let folder = ServerFolder::parse("Zwift_1.0.1_ab").unwrap();
        let path = RelPath::parse(r"data\Worlds\a b#?%.ztx").unwrap();
        assert_eq!(
            remote.file_url(&folder, &path),
            "https://cdn.example/root/Zwift_1.0.1_ab/data/Worlds/a%20b%23%3F%25.ztx"
        );
    }

    #[test]
    fn base_url_needs_an_http_scheme_and_a_host() {
        for bad in ["ftp://x/", "cdn.zwift.com/", "https://", "https:///x", ""] {
            assert!(Remote::new(bad).is_err(), "{bad:?}");
        }
        assert_eq!(Remote::new("http://127.0.0.1:1/r/").unwrap().ver_cur_url(), "http://127.0.0.1:1/r/Zwift_ver_cur.xml");
    }
}
