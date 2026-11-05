//! The graphics profile that `launch` gives the game: the setting, the decision from the
//! GPU test, and the four profile files. docs/graphics.md tells why.

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io;
use std::path::Path;
use std::str::FromStr;

use crate::Error;
use crate::disk::{self, Inspection, Layout};
use crate::model::{FileEntry, Manifest, RelPath};
use crate::sync::io_error;

const SETTING_FILE: &str = "graphics";

/// Each limit is the GPU test time at which the model frame time of Zwift with that profile
/// reaches 16.67 ms (60 FPS). docs/graphics.md has the data and the model.
const LIMITS: [(Profile, f64); 3] = [(Profile::Ultra, 7.25), (Profile::High, 7.84), (Profile::Medium, 8.88)];

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum Profile {
    Basic,
    Medium,
    High,
    Ultra,
}

/// What `zwift-bootstrap graphics` sets. `Auto` runs the GPU test at each launch.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GraphicsSetting {
    Auto,
    Fixed(Profile),
}

#[derive(Clone, PartialEq, Debug)]
pub struct GpuTest {
    /// `GL_RENDERER` of the context that ran the test.
    pub renderer: String,
    /// Median GPU time of one frame of the test workload.
    pub frame_ms: f64,
}

/// What `launch` gave the game.
#[derive(Clone, PartialEq, Debug)]
pub enum GraphicsChoice {
    Fixed(Profile),
    Measured { profile: Profile, test: GpuTest },
    /// Each profile file has its original content, so the GPU table of the game decides.
    GameDefault { reason: String },
}

impl Profile {
    pub const ALL: [Profile; 4] = [Profile::Basic, Profile::Medium, Profile::High, Profile::Ultra];

    pub fn name(self) -> &'static str {
        match self {
            Profile::Basic => "basic",
            Profile::Medium => "medium",
            Profile::High => "high",
            Profile::Ultra => "ultra",
        }
    }

    pub fn for_frame_time(frame_ms: f64) -> Profile {
        LIMITS.iter().find(|(_, limit)| frame_ms <= *limit).map_or(Profile::Basic, |(profile, _)| *profile)
    }

    /// The manifest path of the profile file.
    fn file(self) -> String {
        format!("data/configs/{}.txt", self.name())
    }

    /// The name of the copy of the original file in `.zwift-bootstrap/`.
    fn copy(self) -> String {
        format!("profiles/{}.txt", self.name())
    }
}

impl fmt::Display for Profile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl FromStr for GraphicsSetting {
    type Err = String;

    fn from_str(word: &str) -> Result<GraphicsSetting, String> {
        if word == "auto" {
            return Ok(GraphicsSetting::Auto);
        }
        let profile = Profile::ALL.into_iter().find(|profile| profile.name() == word);
        profile
            .map(GraphicsSetting::Fixed)
            .ok_or_else(|| format!("unknown graphics setting {word:?}; use auto, basic, medium, high or ultra"))
    }
}

impl fmt::Display for GraphicsSetting {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GraphicsSetting::Auto => f.write_str("auto"),
            GraphicsSetting::Fixed(profile) => f.write_str(profile.name()),
        }
    }
}

impl GraphicsChoice {
    /// The profile whose original goes into all four files. `None`: each file gets its own.
    fn profile(&self) -> Option<Profile> {
        match self {
            GraphicsChoice::Fixed(profile) | GraphicsChoice::Measured { profile, .. } => Some(*profile),
            GraphicsChoice::GameDefault { .. } => None,
        }
    }
}

impl fmt::Display for GraphicsChoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GraphicsChoice::Fixed(profile) => write!(f, "{profile} (set with zwift-bootstrap graphics)"),
            GraphicsChoice::Measured { profile, test } => {
                write!(f, "{profile} (GPU test {:.2} ms on {})", test.frame_ms, test.renderer)
            }
            GraphicsChoice::GameDefault { reason } => write!(f, "game default ({reason})"),
        }
    }
}

/// No file is `Auto`.
pub(crate) fn read_setting(layout: &Layout) -> Result<GraphicsSetting, Error> {
    let path = layout.private(SETTING_FILE);
    match fs::read_to_string(&path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(GraphicsSetting::Auto),
        Err(source) => Err(Error::Io { path, source }),
        Ok(text) => text
            .trim()
            .parse()
            .map_err(|reason: String| Error::Io { path, source: io::Error::new(io::ErrorKind::InvalidData, reason) }),
    }
}

pub(crate) fn write_setting(layout: &Layout, setting: GraphicsSetting) -> Result<(), Error> {
    let bytes = format!("{setting}\n");
    layout.write_private_file(SETTING_FILE, bytes.as_bytes()).map_err(io_error(&layout.private(SETTING_FILE)))
}

/// Writes the original of the chosen profile into every profile file that the manifest lists.
/// The originals are copies in `.zwift-bootstrap/`, taken from each file while it still has
/// the content that the manifest lists. `measure` runs only for `Auto`, and only when there
/// is an original to write.
pub(crate) fn prepare(
    layout: &Layout,
    manifest: &Manifest,
    measure: impl FnOnce() -> Result<GpuTest, String>,
) -> Result<GraphicsChoice, Error> {
    let setting = read_setting(layout)?;
    let files: Vec<_> = profile_files(manifest).collect();
    let mut originals = BTreeMap::new();
    for (profile, path, entry) in &files {
        if let Some(bytes) = original(layout, *profile, path, entry)? {
            originals.insert(*profile, bytes);
        }
    }
    let choice = match setting {
        _ if originals.is_empty() => GraphicsChoice::GameDefault { reason: "no original profile files".into() },
        GraphicsSetting::Fixed(profile) => GraphicsChoice::Fixed(profile),
        GraphicsSetting::Auto => match measure() {
            Ok(test) => GraphicsChoice::Measured { profile: Profile::for_frame_time(test.frame_ms), test },
            Err(reason) => GraphicsChoice::GameDefault { reason: format!("GPU test failed: {reason}") },
        },
    };
    let choice = match choice.profile() {
        Some(profile) if !originals.contains_key(&profile) => {
            GraphicsChoice::GameDefault { reason: format!("no original copy of {}", profile.file()) }
        }
        _ => choice,
    };
    for (profile, path, _) in &files {
        let Some(bytes) = originals.get(&choice.profile().unwrap_or(*profile)) else { continue };
        let file = layout.installed(path);
        if !fs::read(&file).is_ok_and(|current| current == *bytes) {
            layout.write_install_file(path.as_str(), bytes).map_err(io_error(&file))?;
        }
    }
    Ok(choice)
}

/// `launch` writes one original into all four profile files, so `verify` accepts a profile
/// file that has the content of any of them.
pub(crate) fn is_swapped_profile(manifest: &Manifest, path: &RelPath, file: &Path) -> bool {
    profile_files(manifest).any(|(_, other, _)| other == path)
        && profile_files(manifest).any(|(_, _, entry)| disk::inspect(file, entry).is_ok_and(|i| i == Inspection::Match))
}

fn profile_files(manifest: &Manifest) -> impl Iterator<Item = (Profile, &RelPath, &FileEntry)> {
    let profile_of = |path: &RelPath| Profile::ALL.into_iter().find(|profile| path.as_str() == profile.file());
    manifest.files().iter().filter_map(move |(path, entry)| Some((profile_of(path)?, path, entry)))
}

/// The installed file while it matches the manifest, which also refreshes the copy (a game
/// update can bring a new original). Else the copy, if there is one.
fn original(layout: &Layout, profile: Profile, path: &RelPath, entry: &FileEntry) -> Result<Option<Vec<u8>>, Error> {
    let file = layout.installed(path);
    let copy = layout.private(&profile.copy());
    if disk::inspect(&file, entry).map_err(io_error(&file))? == Inspection::Match {
        let bytes = fs::read(&file).map_err(io_error(&file))?;
        if !fs::read(&copy).is_ok_and(|current| current == bytes) {
            layout.write_private_file(&profile.copy(), &bytes).map_err(io_error(&copy))?;
        }
        return Ok(Some(bytes));
    }
    match fs::read(&copy) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(Error::Io { path: copy, source }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Checksum, Crc32, ServerFolder, Version};

    /// An install whose four profile files hold the profile name, and its manifest.
    fn install() -> (tempfile::TempDir, Layout, Manifest) {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout { install: dir.path().join("install"), staging: dir.path().join("staging") };
        let entries = Profile::ALL.map(|profile| {
            let path = RelPath::parse(&profile.file()).unwrap();
            let name = profile.name().as_bytes();
            fs::create_dir_all(layout.installed(&path).parent().unwrap()).unwrap();
            fs::write(layout.installed(&path), name).unwrap();
            let checksum = Checksum::Crc(Crc32(crc32fast::hash(name)));
            (path, FileEntry { length: name.len() as u64, checksum, modified: None })
        });
        let manifest = Manifest::new(Version::ZERO, ServerFolder::parse("F").unwrap(), entries.into()).unwrap();
        (dir, layout, manifest)
    }

    fn contents(layout: &Layout) -> [String; 4] {
        Profile::ALL.map(|profile| fs::read_to_string(layout.install.join(profile.file())).unwrap())
    }

    #[test]
    fn auto_writes_the_measured_profile_and_a_failed_test_restores_the_originals() {
        let (_dir, layout, manifest) = install();
        let test = GpuTest { renderer: "Test GPU".into(), frame_ms: 7.5 };
        let choice = prepare(&layout, &manifest, || Ok(test.clone())).unwrap();
        assert_eq!(choice.to_string(), "high (GPU test 7.50 ms on Test GPU)");
        assert_eq!(contents(&layout), ["high"; 4]);

        let choice = prepare(&layout, &manifest, || Err("no libEGL".into())).unwrap();
        assert_eq!(choice.to_string(), "game default (GPU test failed: no libEGL)");
        assert_eq!(contents(&layout), ["basic", "medium", "high", "ultra"]);
    }

    #[test]
    fn the_gpu_test_runs_only_for_auto_with_an_original_to_write() {
        let (_dir, layout, manifest) = install();
        write_setting(&layout, GraphicsSetting::Fixed(Profile::Medium)).unwrap();
        let choice = prepare(&layout, &manifest, || panic!("measured a fixed setting")).unwrap();
        assert_eq!(choice, GraphicsChoice::Fixed(Profile::Medium));

        let empty = Manifest::new(Version::ZERO, ServerFolder::parse("F").unwrap(), Vec::new()).unwrap();
        write_setting(&layout, GraphicsSetting::Auto).unwrap();
        let choice = prepare(&layout, &empty, || panic!("measured with nothing to write")).unwrap();
        assert_eq!(choice, GraphicsChoice::GameDefault { reason: "no original profile files".into() });
    }

    #[test]
    fn the_frame_time_picks_the_highest_profile_whose_limit_it_meets() {
        for (frame_ms, profile) in [
            (0.5, Profile::Ultra),
            (7.25, Profile::Ultra),
            (7.26, Profile::High),
            (7.84, Profile::High),
            (7.85, Profile::Medium),
            (8.88, Profile::Medium),
            (8.89, Profile::Basic),
            (100.0, Profile::Basic),
        ] {
            assert_eq!(Profile::for_frame_time(frame_ms), profile, "{frame_ms} ms");
        }
    }

    #[test]
    fn a_setting_prints_as_the_word_it_parses_from() {
        for (word, setting) in [
            ("auto", GraphicsSetting::Auto),
            ("basic", GraphicsSetting::Fixed(Profile::Basic)),
            ("medium", GraphicsSetting::Fixed(Profile::Medium)),
            ("high", GraphicsSetting::Fixed(Profile::High)),
            ("ultra", GraphicsSetting::Fixed(Profile::Ultra)),
        ] {
            assert_eq!(word.parse::<GraphicsSetting>(), Ok(setting));
            assert_eq!(setting.to_string(), word);
        }
    }

    #[test]
    fn an_unknown_setting_word_is_an_error_that_lists_the_words() {
        for bad in ["", "Ultra", "fast", "ultra high"] {
            let error = bad.parse::<GraphicsSetting>().unwrap_err();
            let lists = error.contains("auto, basic, medium, high or ultra");
            assert!(error.contains(&format!("{bad:?}")) && lists, "{error}");
        }
    }
}
