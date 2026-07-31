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
    "arrow-up",
    "block",
    "check",
    "chevron-down",
    "chevron-right",
    "folder",
    "git-branch",
    "hexagon",
    "list",
    "lock",
    "lock-open",
    "panel-left",
    "pencil",
    "plus",
    "provider-claude",
    "provider-grok",
    "provider-openai",
    "provider-opencode",
    "provider-pi",
    "search",
    "settings",
    "slash",
    "sparkle",
    "star",
    "star-filled",
    "stop",
    "terminal",
    "wrench",
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
