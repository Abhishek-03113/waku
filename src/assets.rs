use std::borrow::Cow;

use anyhow::Result;
use gpui::{AssetSource, SharedString};

/// Icons embedded in the binary so the app stays a single artifact.
pub struct Assets;

macro_rules! icons {
    ($($name:literal),+ $(,)?) => {
        &[$((
            concat!("icons/", $name, ".svg"),
            include_bytes!(concat!("../assets/icons/", $name, ".svg")).as_slice(),
        )),+]
    };
}

const ICONS: &[(&str, &[u8])] = icons![
    "alert",
    "appearance",
    "arrow-left",
    "arrow-right",
    "arrow-up",
    "block",
    "check",
    "chevron-down",
    "chevron-right",
    "copy",
    "folder",
    "folder-new",
    "file-diff",
    "git-branch",
    "globe",
    "hexagon",
    "list",
    "lock",
    "lock-open",
    "panel-left",
    "panel-right",
    "pencil",
    "plus",
    "provider-amp",
    "provider-claude",
    "provider-grok",
    "provider-openai",
    "provider-opencode",
    "provider-pi",
    "rewind",
    "search",
    "settings",
    "slash",
    "sparkle",
    "star",
    "star-filled",
    "stop",
    "terminal",
    "wrench",
    "x",
    "zap",
];

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(ICONS
            .iter()
            .find(|(name, _)| *name == path)
            .map(|(_, bytes)| Cow::Borrowed(*bytes)))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(ICONS
            .iter()
            .filter(|(name, _)| name.starts_with(path))
            .map(|(name, _)| SharedString::from(*name))
            .collect())
    }
}
