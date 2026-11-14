//! Optional local playback: runs librespot so this computer appears as a
//! Spotify Connect speaker that ljud (or any Spotify app) can play on.

use crate::config::{data_dir, Config};
use anyhow::{Context, Result};
use std::fs::{self, File};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

pub struct LocalPlayer {
    child: Child,
}

impl Drop for LocalPlayer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn cache_dir() -> PathBuf {
    data_dir().join("librespot")
}

pub fn has_credentials() -> bool {
    cache_dir().join("credentials.json").exists()
}

fn base_args(cfg: &Config) -> Vec<String> {
    vec![
        "--name".into(),
        cfg.librespot.name.clone(),
        "--device-type".into(),
        "computer".into(),
        "--bitrate".into(),
        cfg.librespot.bitrate.to_string(),
        "--cache".into(),
        cache_dir().display().to_string(),
        "--disable-audio-cache".into(),
    ]
}

/// Start librespot in the background if enabled. Output goes to a log file.
pub fn spawn(cfg: &Config) -> Result<Option<LocalPlayer>> {
    if !cfg.librespot.enabled {
        return Ok(None);
    }
    fs::create_dir_all(cache_dir())?;
    let log_path = data_dir().join("librespot.log");
    let log = File::create(&log_path)?;
    let child = Command::new(&cfg.librespot.path)
        .args(base_args(cfg))
        .args(&cfg.librespot.extra_args)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .spawn()
        .with_context(|| format!("starting {:?} (set [librespot] path in the config)", cfg.librespot.path))?;
    Ok(Some(LocalPlayer { child }))
}

/// One-time interactive login for librespot, so it can register with your
/// account without another Spotify app having to "wake" it first.
pub fn login(cfg: &Config) -> Result<()> {
    fs::create_dir_all(cache_dir())?;
    println!(
        "Starting librespot with browser login. Once it reports that it's authenticated\n\
         and connected, press Ctrl+C. Credentials are cached in {}\n",
        cache_dir().display()
    );
    let status = Command::new(&cfg.librespot.path)
        .args(base_args(cfg))
        .arg("--enable-oauth")
        .status()
        .with_context(|| format!("starting {:?}", cfg.librespot.path))?;
    if has_credentials() {
        println!("\nlibrespot credentials saved. Set `enabled = true` under [librespot] in the config.");
    } else {
        println!("\nlibrespot exited ({status}) without saving credentials.");
    }
    Ok(())
}
