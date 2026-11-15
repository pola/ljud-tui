# ljud

A small terminal Spotify client for controlling Spotify Connect speakers,
with **custom names for speakers**. Written in Rust.

- Search tracks, artists, albums and playlists
- Browse your playlists, albums, artists and Liked songs
- A menu (`m`) on any song, album or artist: add to queue, go to the album or artist(s), play
- The queue (`u`): what's playing and what comes next
- Play on any Spotify Connect speaker on your account/LAN, with play/pause,
  next/previous, seek, volume, shuffle and repeat
- Create, rename and delete playlists; add and remove tracks; like and unlike tracks
- Give speakers your own names (handy when they report a hex string)
- Optional: run `librespot` so this computer is a speaker too

Requires Spotify Premium (Spotify only allows playback control for Premium
accounts) and your own app in the Spotify developer dashboard.

## Setup

1. At <https://developer.spotify.com/dashboard>, create an app (Web API) and
   add the Redirect URI `http://127.0.0.1:8989/login`.
2. Build: `cargo build --release` (the binary is `target/release/ljud`).
3. Put the app's **Client ID** in `~/.config/ljud/config.toml`:
   ```toml
   client_id = "…"
   ```
   No client secret is needed: ljud logs in with PKCE.
4. Run `ljud`. The first time, a browser window opens so you can log in.

The login is saved in `~/.local/share/ljud/token.json` (mode 600).
Run `ljud logout` to remove it.

## Keys

| Key | Action |
|---|---|
| Tab / ← → | switch between library and track list |
| ↑ ↓ / j k, PgUp PgDn, g G | move |
| Enter | open playlist/album/artist, or play the track |
| m | menu for the selected song/album/artist (also Shift+F10 / Menu key) |
| Backspace | back to the previous list |
| u | queue (up next) |
| / | search |
| d | speakers: Enter = play there, e = rename, r = refresh |
| space | play / pause |
| n / p | next / previous |
| + / - | volume |
| < / > | seek −/+ 10 s |
| s / r | shuffle / repeat (off → all → one) |
| N | new playlist |
| e / D | (library) rename / delete playlist |
| a | (track) add to playlist |
| x | (track) remove from playlist or Liked songs |
| L | (track) save to Liked songs |
| R | refresh |
| ? | help |
| q | quit |

## Menus

`m` opens a menu for the selected row (not necessarily the playing song). The
entries come in the same order as in the Android app:

| Row | Menu |
|---|---|
| Song | Add to queue, Go to album, then Go to artist (one per artist) |
| Album | Play album, Add album to queue, Go to album, then Go to artist (one per artist) |
| Artist | Play artist, Go to artist |

An artist page lists popular songs and albums & singles. Spotify blocks its
"top tracks" endpoint for development-mode apps, so "Popular" comes from a
search for the artist's songs (ranked by popularity).

## Queue

`u` shows the queue: the current song and what plays next. Spotify's public API
can add songs to the queue but not remove, reorder or jump to them, so selecting
a queued song opens its menu. Spotify also doesn't mark which upcoming songs were
queued by you and which simply come next in the playlist or album.

## Speaker names

Press `d`, select a speaker, press `e` and type a name. An empty name restores
the original. The names are stored in the config under `[aliases]` (by device id)
and `[name_aliases]` (by the name the device reports, used as a fallback in
case the id changes). `ljud devices` lists the speakers from the command line.

## This computer as a speaker (optional)

```sh
ljud local-login     # one-time browser login for librespot; Ctrl+C when connected
```

Then set `enabled = true` under `[librespot]` in the config. ljud starts
librespot in the background while it runs (log: `~/.local/share/ljud/librespot.log`).

## Commands

```
ljud                 start the player
ljud login | logout
ljud devices         list speakers with your names
ljud local-login     librespot one-time login
ljud selftest        check the API calls (read-only); --write also tests
                     playlist editing on a temporary playlist
```

For debugging, `LJUD_ACCESS_TOKEN=<token>` makes ljud use an existing access token
instead of its own saved login.

## Notes and limits

- Spotify limits developer apps: search returns up to 10 results per type,
  and only users added to the app (in the dashboard, under *User Management*) can log in.
- "Delete playlist" means unfollowing it, which is the only option Spotify offers.
  Spotify can restore deleted playlists from your account page.
- Removing a track from a playlist removes every copy of it in that playlist.
- Liked songs loads at most the first 5000 tracks.
- Chromecast-only (Google Cast) devices are not supported; Spotify Connect speakers are.
