mod api;
mod app;
mod auth;
mod config;
mod local;
mod ui;

use anyhow::{bail, Context, Result};
use config::{config_path, Config};

const HELP: &str = "\
ljud — a small terminal Spotify client for Spotify Connect speakers

USAGE:
    ljud                 start the player (logs in first if needed)
    ljud login           log in to Spotify (again)
    ljud logout          forget the saved login
    ljud devices         list speakers, with your custom names
    ljud local-login     one-time login for librespot (this PC as a speaker)
    ljud selftest        check that the Spotify API calls work (read-only)
    ljud selftest --write   also create, edit and delete a temporary playlist

Config: ~/.config/ljud/config.toml   (press ? inside ljud for keys)
";

fn require_client_id(cfg: &Config) -> Result<()> {
    if cfg.client_id.trim().is_empty() {
        bail!(
            "no client_id configured.\n\n\
             1. Create an app at https://developer.spotify.com/dashboard (Web API)\n\
             2. Add the Redirect URI  http://127.0.0.1:{}/login\n\
             3. Put its Client ID in {} as  client_id = \"...\"",
            cfg.redirect_port,
            config_path().display()
        );
    }
    Ok(())
}

/// Build an API client, logging in interactively if there is no saved session.
fn connect(cfg: &Config) -> Result<api::Api> {
    let http = api::http_client()?;
    // Debug aid: use an externally obtained access token (no refresh).
    if let Ok(t) = std::env::var("LJUD_ACCESS_TOKEN") {
        let token = auth::Token { access_token: t, refresh_token: String::new(), expires_at: u64::MAX };
        return Ok(api::Api::new(http, cfg.client_id.clone(), token));
    }
    require_client_id(cfg)?;
    let token = match auth::load_token() {
        Some(t) => t,
        None => {
            let t = auth::login(cfg, &http)?;
            auth::save_token(&t)?;
            t
        }
    };
    Ok(api::Api::new(http, cfg.client_id.clone(), token))
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cfg = Config::load()?;

    match args.first().map(String::as_str) {
        None => {}
        Some("-h" | "--help" | "help") => {
            print!("{HELP}");
            return Ok(());
        }
        Some("login") => {
            require_client_id(&cfg)?;
            let http = api::http_client()?;
            let t = auth::login(&cfg, &http)?;
            auth::save_token(&t)?;
            let me = api::Api::new(http, cfg.client_id.clone(), t).me()?;
            println!("Logged in as {} ({}). Start the player with `ljud`.", me.name, me.id);
            return Ok(());
        }
        Some("logout") => {
            auth::delete_token()?;
            println!("Saved login removed.");
            return Ok(());
        }
        Some("local-login") => return local::login(&cfg),
        Some("devices") => {
            let api = connect(&cfg)?;
            let devices = api.devices()?;
            if devices.is_empty() {
                println!("No speakers visible right now.");
            }
            for d in devices {
                let alias = cfg.alias_for(d.id.as_deref(), &d.name);
                println!(
                    "{} {:<28} {:<28} {:<10} {}",
                    if d.is_active { "●" } else { " " },
                    alias.unwrap_or(&d.name),
                    if alias.is_some() { d.name.as_str() } else { "" },
                    d.kind,
                    d.id.as_deref().unwrap_or("(no id)")
                );
            }
            return Ok(());
        }
        Some("selftest") => {
            let api = connect(&cfg)?;
            return selftest(&api, args.iter().any(|a| a == "--write"));
        }
        Some(other) => bail!("unknown command {other:?}; see `ljud --help`"),
    }

    let api = connect(&cfg)?;
    let me = api
        .me()
        .context("talking to Spotify failed (try `ljud login`)")?;

    let mut note = None;
    let local = local::spawn(&cfg)?;
    if local.is_some() && !local::has_credentials() {
        note = Some(format!(
            "librespot started, but it has no saved login: run `ljud local-login` once so “{}” shows up as a speaker.",
            cfg.librespot.name
        ));
    }
    let result = app::run(api, cfg, me, note);
    drop(local);
    result
}

fn selftest(api: &api::Api, write: bool) -> Result<()> {
    let mut failures = 0;
    let mut check = |name: &str, r: Result<String>| match r {
        Ok(s) => println!("  ok    {name}: {s}"),
        Err(e) => {
            failures += 1;
            println!("  FAIL  {name}: {e:#}");
        }
    };

    let me = api.me();
    check("me", me.as_ref().map(|u| format!("{} ({:?})", u.id, u.product)).map_err(|e| anyhow::anyhow!("{e:#}")));
    let me = me?;

    let playlists = api.playlists();
    check("playlists", playlists.as_ref().map(|p| format!("{} playlists", p.len())).map_err(|e| anyhow::anyhow!("{e:#}")));
    if let Ok(list) = &playlists {
        if let Some(p) = list.iter().find(|p| p.total > 0) {
            check(
                "playlist tracks",
                api.playlist_tracks(&p.id).map(|t| format!("“{}”: {} of {} tracks, first: {:?}", p.name, t.len(), p.total, t.first().map(|t| &t.name))),
            );
        }
    }
    check("liked songs", api.liked(50).map(|t| format!("{} (first page), first: {:?}", t.len(), t.first().map(|t| &t.name))));
    let search = api.search("abba");
    check(
        "search",
        search.as_ref().map(|r| format!("{} tracks, {} albums, {} playlists", r.tracks.len(), r.albums.len(), r.playlists.len())).map_err(|e| anyhow::anyhow!("{e:#}")),
    );
    if let Some(a) = search.as_ref().ok().and_then(|r| r.albums.first().cloned()) {
        check("album tracks", api.album_tracks(&a).map(|t| format!("“{}”: {} tracks", a.name, t.len())));
    }
    check("devices", api.devices().map(|d| d.iter().map(|d| d.name.clone()).collect::<Vec<_>>().join(", ")));
    check(
        "playback state",
        api.playback().map(|p| match p {
            None => "nothing playing".into(),
            Some(p) => format!("{:?} on {:?}, playing={}", p.track.map(|t| t.name), p.device.map(|d| d.name), p.is_playing),
        }),
    );

    if write {
        println!("  --    write tests (temporary playlist “ljud selftest”)");
        match api.create_playlist(&me.id, "ljud selftest") {
            Ok(p) => {
                check("create playlist", Ok(p.id.clone()));
                check("rename playlist", api.rename_playlist(&p.id, "ljud selftest (renamed)").map(|_| "ok".into()));
                if let Some(t) = search.as_ref().ok().and_then(|r| r.tracks.first().cloned()) {
                    check("add track", api.add_to_playlist(&p.id, std::slice::from_ref(&t.uri)).map(|_| t.name.clone()));
                    check("read back", api.playlist_tracks(&p.id).map(|v| format!("{} track(s)", v.len())));
                    check("remove track", api.remove_from_playlist(&p.id, &t.uri).map(|_| "ok".into()));
                    check("read back", api.playlist_tracks(&p.id).map(|v| format!("{} track(s)", v.len())));
                }
                check("delete playlist", api.unfollow_playlist(&p.id).map(|_| "ok".into()));
            }
            Err(e) => check("create playlist", Err(e)),
        }
    }

    if failures > 0 {
        bail!("{failures} check(s) failed");
    }
    println!("All checks passed.");
    Ok(())
}
