//! The XML boundary. Bytes in, domain types out. Nothing outside this module sees
//! roxmltree, element names, or attribute names.
//!
//! The same parsers read the server copies and the local copies. A local file is no more
//! trusted than a remote one (it may come from an old run or another installer).

use std::time::{Duration, SystemTime};

use roxmltree::{Document, Node};

use crate::model::{
    Checksum, FileEntry, Manifest, ManifestName, ManifestRejection, RelPath, ServerFolder,
    VerCur, Version,
};

/// Parse `Zwift_ver_cur.xml`.
///
/// Root `Zwift`. Required: `version`, `manifest`. Optional: `manifest_checksum`
/// (missing = Absent), `mandatory_version`. Ignored: `sversion`, `gbranch`, `gcommit`,
/// `GAME_URL`, `ver_cur_checksum`.
pub(crate) fn parse_ver_cur(bytes: &[u8]) -> Result<VerCur, String> {
    let text = std::str::from_utf8(bytes).map_err(|e| e.to_string())?;
    let doc = Document::parse(text).map_err(|e| e.to_string())?;
    let root = zwift_root(&doc)?;
    let version = Version::parse(attr(root, "version")?)?;
    let manifest = ManifestName::parse(attr(root, "manifest")?)?;
    let manifest_checksum = match root.attribute("manifest_checksum") {
        Some(text) => Checksum::parse(text)?,
        None => Checksum::Absent,
    };
    let mandatory_version = root.attribute("mandatory_version").map(Version::parse).transpose()?;
    Ok(VerCur::new(version, manifest, manifest_checksum, mandatory_version, bytes.into()))
}

/// Parse a manifest. Root `Zwift` with `version` and `folder`, child `files`, children
/// `file` with `path`, `length`, `checksum`, optional `modified`. Unknown elements are
/// ignored, so new server fields do not break parsing. Set-level rules live in `Manifest::new`.
pub(crate) fn parse_manifest(bytes: &[u8]) -> Result<Manifest, String> {
    let text = std::str::from_utf8(bytes).map_err(|e| e.to_string())?;
    let doc = Document::parse(text).map_err(|e| e.to_string())?;
    let root = zwift_root(&doc)?;
    let version = Version::parse(attr(root, "version")?)?;
    let folder = ServerFolder::parse(attr(root, "folder")?)?;
    let files = only_child(root, "files")?.ok_or("no <files> element")?;
    let mut entries = Vec::new();
    for file in files.children().filter(|n| n.has_tag_name("file")) {
        let raw_path = required_text(file, "path")?;
        let path = RelPath::parse(raw_path).map_err(|e| format!("path {raw_path:?}: {e:?}"))?;
        let length = required_text(file, "length")?;
        let length = length
            .trim()
            .parse::<u64>()
            .map_err(|_| format!("{path}: invalid length {length:?}"))?;
        let checksum =
            Checksum::parse(required_text(file, "checksum")?).map_err(|e| format!("{path}: {e}"))?;
        let modified = only_child(file, "modified")
            .ok()
            .flatten()
            .and_then(|n| n.text())
            .and_then(|t| parse_modified(t.trim()).ok());
        entries.push((path, FileEntry { length, checksum, modified }));
    }
    Manifest::new(version, folder, entries).map_err(|rejection| match rejection {
        ManifestRejection::Duplicate(p) => format!("duplicate path {p}"),
        ManifestRejection::CaseCollision(a, b) => format!("paths {a} and {b} differ only in case"),
        ManifestRejection::FileIsAlsoDir(p) => format!("{p} is both a file and a directory"),
        ManifestRejection::Reserved(p) => format!("path {p} is reserved for zwift-bootstrap"),
    })
}

fn zwift_root<'a>(doc: &'a Document) -> Result<Node<'a, 'a>, String> {
    let root = doc.root_element();
    if !root.has_tag_name("Zwift") {
        return Err(format!("root element is <{}>, not <Zwift>", root.tag_name().name()));
    }
    Ok(root)
}

fn attr<'a>(node: Node<'a, '_>, name: &str) -> Result<&'a str, String> {
    node.attribute(name)
        .ok_or_else(|| format!("<{}> has no {name} attribute", node.tag_name().name()))
}

/// At most one child element with this name.
fn only_child<'a, 'i>(node: Node<'a, 'i>, name: &str) -> Result<Option<Node<'a, 'i>>, String> {
    let mut matches = node.children().filter(|n| n.has_tag_name(name));
    let first = matches.next();
    if matches.next().is_some() {
        return Err(format!("more than one <{name}> element"));
    }
    Ok(first)
}

fn required_text<'a>(node: Node<'a, '_>, name: &str) -> Result<&'a str, String> {
    only_child(node, name)?
        .ok_or_else(|| format!("<file> has no <{name}>"))
        .map(|n| n.text().unwrap_or_default())
}

/// `YYYY-MM-DD HH:MM:SS.mmm`, read as UTC. Days-from-civil arithmetic; no chrono dependency.
fn parse_modified(text: &str) -> Result<SystemTime, String> {
    let bad = || format!("invalid time {text:?}");
    let (date, time) = text.split_once(' ').ok_or_else(bad)?;
    let (hms, millis) = time.split_once('.').unwrap_or((time, "0"));
    let numbers = |s: &str, sep: char| -> Result<Vec<u32>, String> {
        s.split(sep).map(|n| n.parse::<u32>().map_err(|_| bad())).collect()
    };
    let [year, month, day] = numbers(date, '-')?[..] else { return Err(bad()) };
    let [hour, minute, second] = numbers(hms, ':')?[..] else { return Err(bad()) };
    let millis: u32 = millis.parse().map_err(|_| bad())?;
    if !(1970..=9999).contains(&year)
        || !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
        || millis > 999
    {
        return Err(bad());
    }
    let days = days_from_civil(i64::from(year), i64::from(month), i64::from(day));
    let secs = days * 86_400 + i64::from(hour * 3600 + minute * 60 + second);
    let secs = u64::try_from(secs).map_err(|_| bad())?;
    Ok(SystemTime::UNIX_EPOCH + Duration::from_secs(secs) + Duration::from_millis(millis.into()))
}

/// Days since 1970-01-01 in the proleptic Gregorian calendar (Howard Hinnant's algorithm).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month_index = (month + 9) % 12;
    let day_of_year = (153 * month_index + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Crc32;

    const VER_CUR: &str = "<Zwift version=\"1.0.167050\" sversion=\"1.123.0 (167050)\" gbranch=\"rc/1.123.0\" gcommit=\"bd4bbe7a\" GAME_URL=\"https://us-or-rly101.zwift.com\" manifest=\"Zwift_1.0.167050_ce7420cb_manifest.xml\" manifest_checksum=\"667523848\" ver_cur_checksum=\"1134193414\"/>\r\n";

    const MANIFEST: &str = concat!(
        "<Zwift version=\"1.0.167050\" sversion=\"1.123.0 (167050)\" folder=\"Zwift_1.0.167050_ce7420cb\">\r\n",
        "    <files>\r\n",
        "        <file>\r\n",
        "            <path>data\\Worlds\\world9\\map04.ztx</path>\r\n",
        "            <length>60</length>\r\n",
        "            <modified>2026-10-01 17:32:38.235</modified>\r\n",
        "            <checksum>351459945</checksum>\r\n",
        "        </file>\r\n",
        "        <file>\r\n",
        "            <path>ZwiftApp.exe</path>\r\n",
        "            <length>61</length>\r\n",
        "            <modified>yesterday</modified>\r\n",
        "            <checksum>-1</checksum>\r\n",
        "            <future>ignored</future>\r\n",
        "        </file>\r\n",
        "    </files>\r\n",
        "</Zwift>\r\n",
    );

    fn manifest_with_paths(paths: &[&str]) -> String {
        let files: String = paths
            .iter()
            .map(|p| format!("<file><path>{p}</path><length>1</length><checksum>5</checksum></file>"))
            .collect();
        format!("<Zwift version=\"1.0.1\" folder=\"F\"><files>{files}</files></Zwift>")
    }

    #[test]
    fn ver_cur_keeps_served_bytes_verbatim_including_crlf() {
        let v = parse_ver_cur(VER_CUR.as_bytes()).unwrap();
        assert_eq!(v.raw(), VER_CUR.as_bytes());
        assert_eq!(v.version.to_string(), "1.0.167050");
        assert_eq!(v.manifest.as_str(), "Zwift_1.0.167050_ce7420cb_manifest.xml");
        assert_eq!(v.manifest_checksum, Checksum::Crc(Crc32(667523848)));
        assert_eq!(v.mandatory_version, None);
    }

    #[test]
    fn windows_seed_ver_cur_parses_with_absent_checksum() {
        let seed = br#"<Zwift version="0.0.0" GAME_URL="https://us-or-rly101.zwift.com" manifest="Zwift_0.0.0_manifest.xml"/>"#;
        let v = parse_ver_cur(seed).unwrap();
        assert_eq!(v.version, Version::ZERO);
        assert_eq!(v.manifest_checksum, Checksum::Absent);
    }

    #[test]
    fn ver_cur_with_mandatory_version() {
        let xml = br#"<Zwift version="1.0.5" manifest="a_manifest.xml" mandatory_version="1.0.3"/>"#;
        let v = parse_ver_cur(xml).unwrap();
        assert_eq!(v.mandatory_version, Some(Version { major: 1, minor: 0, build: 3 }));
    }

    #[test]
    fn ver_cur_rejects_bad_input() {
        for bad in [
            &br#"<Other version="1.0.0" manifest="a_manifest.xml"/>"#[..],
            br#"<Zwift manifest="a_manifest.xml"/>"#,
            br#"<Zwift version="1.0.0"/>"#,
            br#"<Zwift version="1.0.0" manifest="Zwift_ver_cur.xml"/>"#,
            br#"<Zwift version="1.0.0" manifest="../a_manifest.xml"/>"#,
            br#"<Zwift version="1.0.0" manifest="a_manifest.xml" manifest_checksum="x"/>"#,
            br#"<Zwift version="1.0.0" manifest="a_manifest.xml" mandatory_version="soon"/>"#,
            b"\xff",
            b"<Zwift",
        ] {
            assert!(parse_ver_cur(bad).is_err(), "{}", String::from_utf8_lossy(bad));
        }
    }

    #[test]
    fn manifest_parses_entries_minus_one_checksum_and_modified_time() {
        let m = parse_manifest(MANIFEST.as_bytes()).unwrap();
        assert_eq!(m.version().to_string(), "1.0.167050");
        assert_eq!(m.folder().as_str(), "Zwift_1.0.167050_ce7420cb");
        let files: Vec<_> = m.files().iter().collect();
        assert_eq!(files.len(), 2);
        let (path, map04) = files[1];
        assert_eq!(path.as_str(), "data/Worlds/world9/map04.ztx");
        assert_eq!(map04.length, 60);
        assert_eq!(map04.checksum, Checksum::Crc(Crc32(351459945)));
        let expected = SystemTime::UNIX_EPOCH + Duration::from_millis(1_790_875_958_235);
        assert_eq!(map04.modified, Some(expected));
        let (path, app) = files[0];
        assert_eq!(path.as_str(), "ZwiftApp.exe");
        assert_eq!(app.checksum, Checksum::Absent);
        assert_eq!(app.modified, None);
    }

    #[test]
    fn manifest_rejects_hostile_paths_and_sets() {
        for paths in [
            &[r"..\x"][..],
            &[r"a\..\b"],
            &[r"\abs"],
            &[r"C:\x"],
            &[r"a\\b"],
            &["a/../b"],
            &["Zwift_ver_cur.xml"],
            &[r".zwift-bootstrap\lock"],
            &["A", "a"],
            &["a", r"a\b"],
            &["a&#10;b"],
        ] {
            let xml = manifest_with_paths(paths);
            assert!(parse_manifest(xml.as_bytes()).is_err(), "{paths:?}");
        }
    }

    #[test]
    fn manifest_rejects_malformed_entries() {
        let file = |inner: &str| {
            format!("<Zwift version=\"1.0.1\" folder=\"F\"><files><file>{inner}</file></files></Zwift>")
        };
        for bad in [
            "<Zwift version=\"1.0.1\" folder=\"F\"></Zwift>".to_owned(),
            "<Zwift version=\"1.0.1\" folder=\"a/b\"><files/></Zwift>".to_owned(),
            file("<path>a</path><checksum>1</checksum>"),
            file("<path>a</path><length>-1</length><checksum>1</checksum>"),
            file("<path>a</path><path>b</path><length>1</length><checksum>1</checksum>"),
            "<!DOCTYPE z [<!ENTITY a \"aaaa\">]><Zwift version=\"1.0.1\" folder=\"F\"><files/></Zwift>".to_owned(),
        ] {
            assert!(parse_manifest(bad.as_bytes()).is_err(), "{bad}");
        }
    }

    #[test]
    fn modified_time_is_utc_and_malformed_values_are_errors() {
        assert_eq!(
            parse_modified("2000-02-29 00:00:00.000"),
            Ok(SystemTime::UNIX_EPOCH + Duration::from_secs(951_782_400))
        );
        for bad in ["2026-13-01 00:00:00.000", "2026-10-01", "2026-10-01 25:00:00.0", "1969-12-31 23:59:59.0"] {
            assert!(parse_modified(bad).is_err(), "{bad}");
        }
    }
}
