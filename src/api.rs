//! A thin, blocking Spotify Web API client. Responses are parsed leniently
//! from `serde_json::Value` so small schema changes don't break everything.

use crate::auth::{self, Token};
use anyhow::{anyhow, bail, Result};
use reqwest::blocking::Client;
use reqwest::header::{CONTENT_LENGTH, RETRY_AFTER};
use reqwest::Method;
use serde_json::{json, Value};
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const BASE: &str = "https://api.spotify.com/v1";

pub fn http_client() -> Result<Client> {
    Ok(Client::builder()
        .timeout(Duration::from_secs(20))
        .user_agent(concat!("ljud/", env!("CARGO_PKG_VERSION")))
        .build()?)
}

#[derive(Debug)]
pub struct ApiError {
    pub status: u16,
    pub message: String,
    pub reason: Option<String>,
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Spotify error {}: {}", self.status, self.message)?;
        if let Some(r) = &self.reason {
            write!(f, " ({r})")?;
        }
        Ok(())
    }
}

impl std::error::Error for ApiError {}

pub fn api_status(e: &anyhow::Error) -> Option<u16> {
    e.downcast_ref::<ApiError>().map(|a| a.status)
}

/// True if the error means "there is no active device to play on".
pub fn is_no_device(e: &anyhow::Error) -> bool {
    match e.downcast_ref::<ApiError>() {
        Some(a) => a.status == 404 || a.reason.as_deref() == Some("NO_ACTIVE_DEVICE"),
        None => false,
    }
}

// ---------------------------------------------------------------- models

#[derive(Clone, Debug, PartialEq)]
pub struct User {
    pub id: String,
    pub name: String,
    pub product: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Track {
    pub uri: String,
    pub id: Option<String>,
    pub name: String,
    pub artists: String,
    pub album: String,
    pub duration_ms: u64,
    /// Position in the containing playlist/album (used as play offset).
    pub pos: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Playlist {
    pub id: String,
    pub uri: String,
    pub name: String,
    pub owner_id: String,
    pub owner_name: String,
    pub total: u64,
    pub collaborative: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Album {
    pub id: String,
    pub uri: String,
    pub name: String,
    pub artists: String,
    pub total: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Device {
    pub id: Option<String>,
    pub name: String,
    pub kind: String,
    pub is_active: bool,
    pub is_restricted: bool,
    pub volume: Option<u8>,
}

#[derive(Clone, Debug)]
pub struct Playback {
    pub device: Option<Device>,
    pub is_playing: bool,
    pub progress_ms: u64,
    pub track: Option<Track>,
    pub shuffle: bool,
    pub repeat: String,
}

#[derive(Clone, Debug, Default)]
pub struct SearchResults {
    pub tracks: Vec<Track>,
    pub albums: Vec<Album>,
    pub playlists: Vec<Playlist>,
}

fn s(v: &Value) -> String {
    v.as_str().unwrap_or("").to_string()
}

fn join_artists(v: &Value) -> String {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x["name"].as_str())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default()
}

pub fn parse_track(v: &Value, pos: usize, album: Option<&str>) -> Option<Track> {
    if v.is_null() {
        return None;
    }
    let uri = v["uri"].as_str()?.to_string();
    let artists = if v["artists"].is_array() {
        join_artists(&v["artists"])
    } else {
        s(&v["show"]["name"])
    };
    Some(Track {
        uri,
        id: v["id"].as_str().map(String::from),
        name: v["name"].as_str().unwrap_or("?").to_string(),
        artists,
        album: album
            .map(String::from)
            .unwrap_or_else(|| s(&v["album"]["name"])),
        duration_ms: v["duration_ms"].as_u64().unwrap_or(0),
        pos,
    })
}

fn parse_playlist(v: &Value) -> Option<Playlist> {
    if v.is_null() {
        return None;
    }
    Some(Playlist {
        id: v["id"].as_str()?.to_string(),
        uri: s(&v["uri"]),
        name: s(&v["name"]),
        owner_id: s(&v["owner"]["id"]),
        owner_name: v["owner"]["display_name"]
            .as_str()
            .or(v["owner"]["id"].as_str())
            .unwrap_or("")
            .to_string(),
        total: v["tracks"]["total"]
            .as_u64()
            .or_else(|| v["items"]["total"].as_u64())
            .unwrap_or(0),
        collaborative: v["collaborative"].as_bool().unwrap_or(false),
    })
}

fn parse_album(v: &Value) -> Option<Album> {
    if v.is_null() {
        return None;
    }
    Some(Album {
        id: v["id"].as_str()?.to_string(),
        uri: s(&v["uri"]),
        name: s(&v["name"]),
        artists: join_artists(&v["artists"]),
        total: v["total_tracks"].as_u64().unwrap_or(0),
    })
}

fn parse_device(v: &Value) -> Device {
    Device {
        id: v["id"].as_str().map(String::from),
        name: s(&v["name"]),
        kind: s(&v["type"]),
        is_active: v["is_active"].as_bool().unwrap_or(false),
        is_restricted: v["is_restricted"].as_bool().unwrap_or(false),
        volume: v["volume_percent"].as_u64().map(|x| x.min(100) as u8),
    }
}

/// Playlist item objects hold the track under "track" (classic) or "item" (newer API).
fn playlist_item_track(it: &Value) -> &Value {
    match it.get("track") {
        Some(t) if !t.is_null() => t,
        _ => it.get("item").unwrap_or(&Value::Null),
    }
}

// ---------------------------------------------------------------- client

#[derive(Clone)]
pub struct Api {
    http: Client,
    client_id: String,
    token: Arc<Mutex<Token>>,
}

impl Api {
    pub fn new(http: Client, client_id: String, token: Token) -> Self {
        Api {
            http,
            client_id,
            token: Arc::new(Mutex::new(token)),
        }
    }

    fn access_token(&self, force_refresh: bool) -> Result<String> {
        let mut t = self.token.lock().unwrap();
        if force_refresh || t.expires_at <= auth::now() + 60 {
            let new = auth::refresh(&self.http, &self.client_id, &t.refresh_token)?;
            auth::save_token(&new)?;
            *t = new;
        }
        Ok(t.access_token.clone())
    }

    /// Perform a request. Returns `None` for empty (e.g. 204) responses.
    pub fn call(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<&Value>,
    ) -> Result<Option<Value>> {
        let url = if path.starts_with("http") {
            path.to_string()
        } else {
            format!("{BASE}{path}")
        };
        let mut force_refresh = false;
        for _ in 0..4 {
            let token = self.access_token(force_refresh)?;
            let mut rb = self
                .http
                .request(method.clone(), &url)
                .bearer_auth(&token)
                .query(query);
            rb = match body {
                Some(b) => rb.json(b),
                None if method != Method::GET => rb.header(CONTENT_LENGTH, "0"),
                None => rb,
            };
            let resp = rb.send()?;
            let status = resp.status();
            if status.as_u16() == 401 && !force_refresh {
                force_refresh = true;
                continue;
            }
            if status.as_u16() == 429 {
                let secs = resp
                    .headers()
                    .get(RETRY_AFTER)
                    .and_then(|h| h.to_str().ok())
                    .and_then(|h| h.parse::<u64>().ok())
                    .unwrap_or(1)
                    .clamp(1, 10);
                std::thread::sleep(Duration::from_secs(secs));
                continue;
            }
            let text = resp.text()?;
            if status.is_success() {
                if text.trim().is_empty() {
                    return Ok(None);
                }
                return Ok(serde_json::from_str(&text).ok());
            }
            let parsed: Option<Value> = serde_json::from_str(&text).ok();
            let err = parsed.as_ref().map(|v| &v["error"]);
            let message = err
                .and_then(|e| e["message"].as_str().or(e.as_str()))
                .map(String::from)
                .unwrap_or_else(|| text.chars().take(200).collect());
            let reason = err.and_then(|e| e["reason"].as_str()).map(String::from);
            return Err(ApiError {
                status: status.as_u16(),
                message,
                reason,
            }
            .into());
        }
        bail!("Spotify kept rejecting or rate-limiting the request to {path}")
    }

    fn get(&self, path: &str, query: &[(&str, String)]) -> Result<Value> {
        self.call(Method::GET, path, query, None)?
            .ok_or_else(|| anyhow!("empty response from {path}"))
    }

    /// Fetch a paged collection, following `next` links, up to `max` items.
    fn paged(&self, path: &str, query: &[(&str, String)], max: usize) -> Result<Vec<Value>> {
        let mut out = Vec::new();
        let mut page = self.get(path, query)?;
        loop {
            // Search results nest the page one level down; callers pass the page itself.
            if let Some(items) = page["items"].as_array() {
                out.extend(items.iter().cloned());
            }
            if out.len() >= max {
                out.truncate(max);
                break;
            }
            match page["next"].as_str() {
                Some(next) => page = self.get(next, &[])?,
                None => break,
            }
        }
        Ok(out)
    }

    /// Try `primary`; if Spotify says that endpoint doesn't exist/isn't allowed, try `fallback`.
    fn either<T>(
        &self,
        primary: impl FnOnce() -> Result<T>,
        fallback: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        match primary() {
            Err(e) if matches!(api_status(&e), Some(403 | 404 | 405 | 410)) => {
                fallback().map_err(|e2| e2.context(format!("{e:#}")))
            }
            other => other,
        }
    }

    // ------------------------------------------------------------ library

    pub fn me(&self) -> Result<User> {
        let v = self.get("/me", &[])?;
        Ok(User {
            id: s(&v["id"]),
            name: v["display_name"]
                .as_str()
                .or(v["id"].as_str())
                .unwrap_or("")
                .to_string(),
            product: v["product"].as_str().map(String::from),
        })
    }

    pub fn playlists(&self) -> Result<Vec<Playlist>> {
        let items = self.paged("/me/playlists", &[("limit", "50".into())], 2000)?;
        Ok(items.iter().filter_map(parse_playlist).collect())
    }

    pub fn playlist_tracks(&self, id: &str) -> Result<Vec<Track>> {
        let q = [("limit", "100".to_string()), ("additional_types", "track".to_string())];
        let items = self.either(
            || self.paged(&format!("/playlists/{id}/items"), &q, 10_000),
            || self.paged(&format!("/playlists/{id}/tracks"), &q, 10_000),
        )?;
        Ok(items
            .iter()
            .enumerate()
            .filter_map(|(i, it)| parse_track(playlist_item_track(it), i, None))
            .collect())
    }

    pub fn liked(&self, max: usize) -> Result<Vec<Track>> {
        let items = self.paged("/me/tracks", &[("limit", "50".into())], max)?;
        Ok(items
            .iter()
            .enumerate()
            .filter_map(|(i, it)| parse_track(playlist_item_track(it), i, None))
            .collect())
    }

    pub fn album_tracks(&self, album: &Album) -> Result<Vec<Track>> {
        let items = self.paged(
            &format!("/albums/{}/tracks", album.id),
            &[("limit", "50".into())],
            2000,
        )?;
        Ok(items
            .iter()
            .enumerate()
            .filter_map(|(i, t)| parse_track(t, i, Some(&album.name)))
            .collect())
    }

    pub fn search(&self, query: &str) -> Result<SearchResults> {
        let run = |limit: &str| {
            self.get(
                "/search",
                &[
                    ("q", query.to_string()),
                    ("type", "track,album,playlist".into()),
                    ("limit", limit.into()),
                ],
            )
        };
        // Some app tiers cap the search limit; retry smaller if the first is refused.
        let v = match run("20") {
            Err(e) if api_status(&e) == Some(400) => run("10")?,
            other => other?,
        };
        let list = |key: &str| v[key]["items"].as_array().cloned().unwrap_or_default();
        Ok(SearchResults {
            tracks: list("tracks")
                .iter()
                .enumerate()
                .filter_map(|(i, t)| parse_track(t, i, None))
                .collect(),
            albums: list("albums").iter().filter_map(parse_album).collect(),
            playlists: list("playlists").iter().filter_map(parse_playlist).collect(),
        })
    }

    // ------------------------------------------------------------ playback

    pub fn playback(&self) -> Result<Option<Playback>> {
        let Some(v) = self.call(
            Method::GET,
            "/me/player",
            &[("additional_types", "track,episode".into())],
            None,
        )?
        else {
            return Ok(None);
        };
        Ok(Some(Playback {
            device: v.get("device").filter(|d| !d.is_null()).map(parse_device),
            is_playing: v["is_playing"].as_bool().unwrap_or(false),
            progress_ms: v["progress_ms"].as_u64().unwrap_or(0),
            track: parse_track(&v["item"], 0, None),
            shuffle: v["shuffle_state"].as_bool().unwrap_or(false),
            repeat: v["repeat_state"].as_str().unwrap_or("off").to_string(),
        }))
    }

    pub fn devices(&self) -> Result<Vec<Device>> {
        let v = self.get("/me/player/devices", &[])?;
        Ok(v["devices"]
            .as_array()
            .map(|a| a.iter().map(parse_device).collect())
            .unwrap_or_default())
    }

    fn device_q(device: Option<&str>) -> Vec<(&'static str, String)> {
        device.map(|d| vec![("device_id", d.to_string())]).unwrap_or_default()
    }

    /// Start playback. `body` is e.g. `{"context_uri":..,"offset":..}` or
    /// `{"uris":[..]}`; `None` resumes whatever was playing.
    pub fn play(&self, device: Option<&str>, body: Option<&Value>) -> Result<()> {
        self.call(Method::PUT, "/me/player/play", &Self::device_q(device), body)?;
        Ok(())
    }

    pub fn pause(&self) -> Result<()> {
        self.call(Method::PUT, "/me/player/pause", &[], None)?;
        Ok(())
    }

    pub fn next(&self) -> Result<()> {
        self.call(Method::POST, "/me/player/next", &[], None)?;
        Ok(())
    }

    pub fn previous(&self) -> Result<()> {
        self.call(Method::POST, "/me/player/previous", &[], None)?;
        Ok(())
    }

    pub fn seek(&self, position_ms: u64) -> Result<()> {
        self.call(
            Method::PUT,
            "/me/player/seek",
            &[("position_ms", position_ms.to_string())],
            None,
        )?;
        Ok(())
    }

    pub fn volume(&self, percent: u8) -> Result<()> {
        self.call(
            Method::PUT,
            "/me/player/volume",
            &[("volume_percent", percent.min(100).to_string())],
            None,
        )?;
        Ok(())
    }

    pub fn shuffle(&self, on: bool) -> Result<()> {
        self.call(Method::PUT, "/me/player/shuffle", &[("state", on.to_string())], None)?;
        Ok(())
    }

    pub fn repeat(&self, mode: &str) -> Result<()> {
        self.call(Method::PUT, "/me/player/repeat", &[("state", mode.to_string())], None)?;
        Ok(())
    }

    pub fn transfer(&self, device_id: &str, play: bool) -> Result<()> {
        self.call(
            Method::PUT,
            "/me/player",
            &[],
            Some(&json!({ "device_ids": [device_id], "play": play })),
        )?;
        Ok(())
    }

    // ------------------------------------------------------------ editing

    pub fn create_playlist(&self, user_id: &str, name: &str) -> Result<Playlist> {
        let body = json!({ "name": name, "public": false, "description": "" });
        let v = self.either(
            || self.call(Method::POST, &format!("/users/{user_id}/playlists"), &[], Some(&body)),
            || self.call(Method::POST, "/me/playlists", &[], Some(&body)),
        )?;
        v.as_ref()
            .and_then(parse_playlist)
            .ok_or_else(|| anyhow!("Spotify did not return the new playlist"))
    }

    pub fn rename_playlist(&self, id: &str, name: &str) -> Result<()> {
        self.call(Method::PUT, &format!("/playlists/{id}"), &[], Some(&json!({ "name": name })))?;
        Ok(())
    }

    /// "Delete" a playlist. Spotify only supports unfollowing; for your own
    /// playlists that removes it from your library.
    pub fn unfollow_playlist(&self, id: &str) -> Result<()> {
        self.call(Method::DELETE, &format!("/playlists/{id}/followers"), &[], None)?;
        Ok(())
    }

    pub fn add_to_playlist(&self, id: &str, uris: &[String]) -> Result<()> {
        let body = json!({ "uris": uris });
        self.either(
            || self.call(Method::POST, &format!("/playlists/{id}/items"), &[], Some(&body)),
            || self.call(Method::POST, &format!("/playlists/{id}/tracks"), &[], Some(&body)),
        )?;
        Ok(())
    }

    /// Remove all occurrences of a track from a playlist.
    pub fn remove_from_playlist(&self, id: &str, uri: &str) -> Result<()> {
        let items = json!({ "items": [{ "uri": uri }] });
        let tracks = json!({ "tracks": [{ "uri": uri }] });
        self.either(
            || self.call(Method::DELETE, &format!("/playlists/{id}/items"), &[], Some(&items)),
            || self.call(Method::DELETE, &format!("/playlists/{id}/tracks"), &[], Some(&tracks)),
        )?;
        Ok(())
    }

    pub fn save_track(&self, track: &Track, save: bool) -> Result<()> {
        let method = if save { Method::PUT } else { Method::DELETE };
        let id = track
            .id
            .clone()
            .ok_or_else(|| anyhow!("this track can't be saved (no Spotify id)"))?;
        self.either(
            || self.call(method.clone(), "/me/tracks", &[("ids", id.clone())], None),
            || self.call(method.clone(), "/me/library", &[("uris", track.uri.clone())], None),
        )?;
        Ok(())
    }
}
