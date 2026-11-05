//! The graphics profile that `launch` gives the game: the setting, the decision from the
//! GPU test, and the four profile files. docs/graphics.md tells why.

use std::fmt;
use std::str::FromStr;

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
        todo!("{frame_ms}")
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
        todo!("{word}")
    }
}

impl fmt::Display for GraphicsSetting {
    fn fmt(&self, _f: &mut fmt::Formatter<'_>) -> fmt::Result {
        todo!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
