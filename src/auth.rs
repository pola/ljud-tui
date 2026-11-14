//! Spotify login using the Authorization Code flow with PKCE.
//! Only the (public) client ID is needed; no client secret.

use crate::config::{data_dir, Config};
use anyhow::{anyhow, bail, Context, Result};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use rand::distributions::Alphanumeric;
use rand::Rng;
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const AUTHORIZE_URL: &str = "https://accounts.spotify.com/authorize";
const TOKEN_URL: &str = "https://accounts.spotify.com/api/token";

pub const SCOPES: &[&str] = &[
    "user-read-private",
    "user-read-playback-state",
    "user-modify-playback-state",
    "user-read-currently-playing",
    "playlist-read-private",
    "playlist-read-collaborative",
    "playlist-modify-private",
    "playlist-modify-public",
    "user-library-read",
    "user-library-modify",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Token {
    pub access_token: String,
    pub refresh_token: String,
    /// Unix time (seconds) when the access token expires.
    pub expires_at: u64,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: u64,
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn token_path() -> PathBuf {
    data_dir().join("token.json")
}

pub fn load_token() -> Option<Token> {
    let text = fs::read_to_string(token_path()).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn save_token(token: &Token) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    fs::create_dir_all(data_dir())?;
    let path = token_path();
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&path)
        .with_context(|| format!("writing {}", path.display()))?;
    f.write_all(serde_json::to_string_pretty(token)?.as_bytes())?;
    Ok(())
}

pub fn delete_token() -> Result<()> {
    let path = token_path();
    if path.exists() {
        fs::remove_file(&path)?;
    }
    Ok(())
}

fn random_string(len: usize) -> String {
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(len)
        .map(char::from)
        .collect()
}

/// Interactive browser login. Blocks until the browser hits the redirect URI.
pub fn login(cfg: &Config, http: &Client) -> Result<Token> {
    let verifier = random_string(64);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let state = random_string(16);
    let redirect_uri = format!("http://127.0.0.1:{}/login", cfg.redirect_port);

    let mut url = url::Url::parse(AUTHORIZE_URL)?;
    url.query_pairs_mut()
        .append_pair("client_id", &cfg.client_id)
        .append_pair("response_type", "code")
        .append_pair("redirect_uri", &redirect_uri)
        .append_pair("code_challenge_method", "S256")
        .append_pair("code_challenge", &challenge)
        .append_pair("state", &state)
        .append_pair("scope", &SCOPES.join(" "));

    let listener = TcpListener::bind(("127.0.0.1", cfg.redirect_port)).with_context(|| {
        format!(
            "could not listen on 127.0.0.1:{} for the login redirect \
             (is another program, or another ljud, using that port?)",
            cfg.redirect_port
        )
    })?;

    eprintln!("Log in to Spotify in your browser. If it doesn't open by itself, visit:\n\n{url}\n");
    let _ = open::that_detached(url.as_str());

    let code = loop {
        let (mut stream, _) = listener.accept()?;
        // Browsers open speculative connections that never send anything.
        stream.set_read_timeout(Some(Duration::from_secs(3)))?;
        let mut line = String::new();
        if BufReader::new(&stream).read_line(&mut line).is_err() {
            continue;
        }
        let path = line.split_whitespace().nth(1).unwrap_or("");
        if !path.starts_with("/login") {
            let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n");
            continue;
        }
        let parsed = url::Url::parse(&format!("http://127.0.0.1{path}"))?;
        let get = |key: &str| {
            parsed
                .query_pairs()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.into_owned())
        };
        let (ok, body) = match (get("code"), get("error")) {
            (Some(_), _) if get("state").as_deref() != Some(state.as_str()) => {
                (false, "Login failed: state mismatch. Try again.".to_string())
            }
            (Some(_), _) => (true, "Logged in to ljud. You can close this tab.".to_string()),
            (None, Some(e)) => (false, format!("Login failed: {e}")),
            (None, None) => (false, "Login failed: no code in redirect.".to_string()),
        };
        let html = format!(
            "<!doctype html><meta charset=utf-8><title>ljud</title>\
             <body style=\"font-family:sans-serif;margin:3em\"><h2>{body}</h2></body>"
        );
        let _ = write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{html}",
            html.len()
        );
        if !ok {
            bail!("{body}");
        }
        break get("code").unwrap();
    };

    let resp = http
        .post(TOKEN_URL)
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("redirect_uri", redirect_uri.as_str()),
            ("client_id", cfg.client_id.as_str()),
            ("code_verifier", verifier.as_str()),
        ])
        .send()?;
    let token = parse_token_response(resp, None)?;
    Ok(token)
}

pub fn refresh(http: &Client, client_id: &str, refresh_token: &str) -> Result<Token> {
    let resp = http
        .post(TOKEN_URL)
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", client_id),
        ])
        .send()?;
    parse_token_response(resp, Some(refresh_token)).map_err(|e| {
        e.context("refreshing the Spotify session failed; run `ljud login` to log in again")
    })
}

fn parse_token_response(resp: reqwest::blocking::Response, old_refresh: Option<&str>) -> Result<Token> {
    let status = resp.status();
    let text = resp.text()?;
    if !status.is_success() {
        return Err(anyhow!("Spotify token endpoint returned {status}: {text}"));
    }
    let tr: TokenResponse = serde_json::from_str(&text).context("unexpected token response")?;
    let refresh_token = tr
        .refresh_token
        .or_else(|| old_refresh.map(String::from))
        .ok_or_else(|| anyhow!("Spotify did not return a refresh token"))?;
    Ok(Token {
        access_token: tr.access_token,
        refresh_token,
        expires_at: now() + tr.expires_in,
    })
}
