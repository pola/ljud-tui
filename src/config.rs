use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

/// User configuration, stored in `~/.config/ljud/config.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Client ID of your own app from https://developer.spotify.com/dashboard
    pub client_id: String,
    /// Port of the login redirect, i.e. `http://127.0.0.1:<port>/login`.
    /// That exact URI must be registered as a Redirect URI in your Spotify app.
    pub redirect_port: u16,
    /// Album art in the now-playing bar: "blocks" (color), "ascii" or "off".
    pub cover: CoverMode,
    pub librespot: Librespot,
    /// Custom speaker names, keyed by Spotify Connect device id.
    pub aliases: BTreeMap<String, String>,
    /// Custom speaker names keyed by the name the device reports. Used as a
    /// fallback in case a speaker's device id changes.
    pub name_aliases: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CoverMode {
    /// Two color pixels per character cell ("▀" with foreground/background colors).
    #[default]
    Blocks,
    /// Monochrome characters by brightness.
    Ascii,
    Off,
}

impl CoverMode {
    pub fn next(self) -> Self {
        match self {
            CoverMode::Blocks => CoverMode::Ascii,
            CoverMode::Ascii => CoverMode::Off,
            CoverMode::Off => CoverMode::Blocks,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            CoverMode::Blocks => "color blocks",
            CoverMode::Ascii => "ASCII",
            CoverMode::Off => "off",
        }
    }
}

/// Optional: run librespot so this computer shows up as a speaker.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Librespot {
    pub enabled: bool,
    pub path: String,
    pub name: String,
    pub bitrate: u32,
    pub extra_args: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            client_id: String::new(),
            redirect_port: 8989,
            cover: CoverMode::default(),
            librespot: Librespot::default(),
            aliases: BTreeMap::new(),
            name_aliases: BTreeMap::new(),
        }
    }
}

impl Default for Librespot {
    fn default() -> Self {
        Librespot {
            enabled: false,
            path: "librespot".into(),
            name: "ljud".into(),
            bitrate: 320,
            extra_args: Vec::new(),
        }
    }
}

pub fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("ljud")
}

pub fn data_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("ljud")
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.toml")
}

const HEADER: &str = "\
# ljud configuration. Edited by hand or by ljud itself (speaker renames).
#
# client_id:     Client ID of your app at https://developer.spotify.com/dashboard
#                Its Redirect URIs must include http://127.0.0.1:<redirect_port>/login
# cover:         album art in the now-playing bar: blocks, ascii or off (key c)
# [librespot]:   set enabled = true to make this computer a speaker
#                (run `ljud local-login` once first).
# [aliases]:     custom speaker names, keyed by device id (rename with `e` in the
#                speaker list). [name_aliases] does the same keyed by reported name.

";

impl Config {
    pub fn load() -> Result<Self> {
        let path = config_path();
        if !path.exists() {
            let cfg = Config::default();
            cfg.save()?;
            return Ok(cfg);
        }
        let text =
            fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }

    pub fn save(&self) -> Result<()> {
        fs::create_dir_all(config_dir())?;
        let path = config_path();
        let tmp = path.with_extension("toml.tmp");
        let body = toml::to_string_pretty(self)?;
        fs::write(&tmp, format!("{HEADER}{body}"))?;
        fs::rename(&tmp, &path).with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }

    /// The custom name for a device, if one has been set.
    pub fn alias_for(&self, id: Option<&str>, name: &str) -> Option<&str> {
        id.and_then(|id| self.aliases.get(id))
            .or_else(|| self.name_aliases.get(name))
            .map(String::as_str)
    }

    /// Set (or clear, with an empty string) the custom name for a device.
    pub fn set_alias(&mut self, id: Option<&str>, name: &str, alias: &str) {
        let alias = alias.trim();
        if alias.is_empty() {
            if let Some(id) = id {
                self.aliases.remove(id);
            }
            self.name_aliases.remove(name);
        } else {
            if let Some(id) = id {
                self.aliases.insert(id.to_string(), alias.to_string());
            }
            self.name_aliases.insert(name.to_string(), alias.to_string());
        }
    }
}
