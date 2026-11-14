use crate::app::{App, Focus, Popup, Row};
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Cell, Clear, List, ListItem, Paragraph, Row as TRow, Table, Wrap};
use ratatui::Frame;
use std::time::Duration;

const ACCENT: Color = Color::Green;

fn fmt_ms(ms: u64) -> String {
    let s = ms / 1000;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

fn pane(title: String, focused: bool) -> Block<'static> {
    let b = Block::bordered().border_type(BorderType::Rounded).title(title);
    if focused {
        b.border_style(Style::new().fg(ACCENT))
    } else {
        b.border_style(Style::new().fg(Color::DarkGray))
    }
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let [main, bar, status] = Layout::vertical([
        Constraint::Min(6),
        Constraint::Length(4),
        Constraint::Length(1),
    ])
    .areas(f.area());
    let [left, right] =
        Layout::horizontal([Constraint::Percentage(28), Constraint::Percentage(72)]).areas(main);

    draw_library(f, app, left);
    draw_rows(f, app, right);
    draw_bar(f, app, bar);
    draw_status(f, app, status);
    draw_popup(f, app);
}

fn draw_library(f: &mut Frame, app: &mut App, area: Rect) {
    let mut items = vec![ListItem::new(Line::from("♥ Liked songs".bold()))];
    for p in &app.playlists {
        let mut spans = vec![Span::raw(p.name.clone())];
        spans.push(Span::styled(format!("  {}", p.total), Style::new().fg(Color::DarkGray)));
        if p.owner_id != app.me.id {
            spans.push(Span::styled(format!(" · {}", p.owner_name), Style::new().fg(Color::DarkGray)));
        }
        items.push(ListItem::new(Line::from(spans)));
    }
    let list = List::new(items)
        .block(pane(" Library ".into(), app.focus == Focus::Library))
        .highlight_style(Style::new().add_modifier(Modifier::REVERSED))
        .highlight_symbol("▌");
    f.render_stateful_widget(list, area, &mut app.lib_state);
}

fn draw_rows(f: &mut Frame, app: &mut App, area: Rect) {
    let playing_uri = app
        .playback
        .as_ref()
        .and_then(|p| p.track.as_ref())
        .map(|t| t.uri.clone());
    let dim = Style::new().fg(Color::DarkGray);

    let mut n = 0;
    let rows: Vec<TRow> = app
        .rows
        .iter()
        .map(|r| match r {
            Row::Header(h) => TRow::new(vec![Cell::from(""), Cell::from(h.clone().bold().fg(ACCENT))]),
            Row::Track(t) => {
                n += 1;
                let style = if playing_uri.as_deref() == Some(t.uri.as_str()) {
                    Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)
                } else {
                    Style::new()
                };
                TRow::new(vec![
                    Cell::from(Span::styled(format!("{n}"), dim)),
                    Cell::from(t.name.clone()),
                    Cell::from(t.artists.clone()),
                    Cell::from(t.album.clone()),
                    Cell::from(Span::styled(fmt_ms(t.duration_ms), dim)),
                ])
                .style(style)
            }
            Row::Album(a) => TRow::new(vec![
                Cell::from(Span::styled("◎", dim)),
                Cell::from(a.name.clone()),
                Cell::from(a.artists.clone()),
                Cell::from(Span::styled("album", dim)),
                Cell::from(Span::styled(format!("{} tr", a.total), dim)),
            ]),
            Row::Playlist(p) => TRow::new(vec![
                Cell::from(Span::styled("♫", dim)),
                Cell::from(p.name.clone()),
                Cell::from(p.owner_name.clone()),
                Cell::from(Span::styled("playlist", dim)),
                Cell::from(Span::styled(format!("{} tr", p.total), dim)),
            ]),
        })
        .collect();

    let mut title = format!(" {} ", app.view_title());
    if app.loading {
        title.push_str("· loading… ");
    } else if n > 0 {
        title.push_str(&format!("· {n} tracks "));
    }
    let empty = !app.loading && app.rows.is_empty();

    let table = Table::new(
        rows,
        [
            Constraint::Length(5),
            Constraint::Fill(4),
            Constraint::Fill(3),
            Constraint::Fill(3),
            Constraint::Length(7),
        ],
    )
    .header(TRow::new(vec!["#", "Title", "Artist", "Album", "Time"]).style(dim.add_modifier(Modifier::BOLD)))
    .block(pane(title, app.focus == Focus::Rows))
    .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED))
    .highlight_symbol("▌");
    f.render_stateful_widget(table, area, &mut app.rows_state);

    if empty {
        let inner = area.inner(ratatui::layout::Margin::new(2, 3));
        f.render_widget(Paragraph::new("Nothing here.").style(dim), inner);
    }
}

fn draw_bar(f: &mut Frame, app: &App, area: Rect) {
    let dim = Style::new().fg(Color::DarkGray);
    let Some(p) = &app.playback else {
        let block = pane(" Not playing ".into(), false);
        let text = Paragraph::new(vec![
            Line::from("Nothing is playing. Pick a track and press Enter, or press d to choose a speaker."),
        ])
        .style(dim)
        .block(block);
        f.render_widget(text, area);
        return;
    };

    let device = p
        .device
        .as_ref()
        .map(|d| app.device_label(d))
        .unwrap_or_else(|| "unknown speaker".into());
    let mut title_spans = vec![Span::raw(" ♪ "), Span::styled(device, Style::new().fg(ACCENT).bold())];
    if let Some(v) = p.device.as_ref().and_then(|d| d.volume) {
        title_spans.push(Span::styled(format!("  vol {v}%"), dim));
    }
    if p.shuffle {
        title_spans.push(Span::raw("  shuffle"));
    }
    match p.repeat.as_str() {
        "context" => title_spans.push(Span::raw("  repeat")),
        "track" => title_spans.push(Span::raw("  repeat one")),
        _ => {}
    }
    title_spans.push(Span::raw(" "));
    let block = pane(String::new(), false).title(Line::from(title_spans));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let icon = if p.is_playing { "▶" } else { "❚❚" };
    let line1 = match &p.track {
        Some(t) => Line::from(vec![
            Span::styled(format!("{icon} "), Style::new().fg(ACCENT)),
            Span::styled(t.name.clone(), Style::new().bold()),
            Span::raw(format!("  {}", t.artists)),
            Span::styled(
                if t.album.is_empty() { String::new() } else { format!(" · {}", t.album) },
                dim,
            ),
        ]),
        None => Line::from(format!("{icon} (unknown track)")),
    };

    let dur = p.track.as_ref().map(|t| t.duration_ms).unwrap_or(0);
    let pos = app.current_progress_ms();
    let times = format!(" {} / {}", fmt_ms(pos), fmt_ms(dur));
    let bar_w = (inner.width as usize).saturating_sub(times.chars().count() + 1);
    let filled = (bar_w as u64 * pos.min(dur)).checked_div(dur).unwrap_or(0) as usize;
    let line2 = Line::from(vec![
        Span::styled("━".repeat(filled), Style::new().fg(ACCENT)),
        Span::styled("─".repeat(bar_w - filled), dim),
        Span::raw(times),
    ]);
    f.render_widget(Paragraph::new(vec![line1, line2]), inner);
}

fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    let line = match &app.status {
        Some((msg, is_err, at)) if at.elapsed() < Duration::from_secs(if *is_err { 10 } else { 5 }) => {
            let style = if *is_err { Style::new().fg(Color::Red) } else { Style::new().fg(ACCENT) };
            Line::from(Span::styled(format!(" {msg}"), style))
        }
        _ => Line::from(Span::styled(
            format!(
                " {} · ? help · / search · d speakers · space play/pause · n/p next/prev · q quit",
                app.me.name
            ),
            Style::new().fg(Color::DarkGray),
        )),
    };
    f.render_widget(Paragraph::new(line), area);
}

fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let [r] = Layout::vertical([Constraint::Length(h.min(area.height))]).flex(Flex::Center).areas(area);
    let [r] = Layout::horizontal([Constraint::Length(w.min(area.width))]).flex(Flex::Center).areas(r);
    r
}

const HELP: &[(&str, &str)] = &[
    ("Tab / ← →", "switch between library and track list"),
    ("↑ ↓  j k  PgUp PgDn  g G", "move"),
    ("Enter", "open playlist/album · play track"),
    ("/", "search tracks, albums, playlists"),
    ("d", "speakers: Enter = play there, e = rename, r = refresh"),
    ("space", "play / pause"),
    ("n  p", "next / previous track"),
    ("+  -", "volume"),
    ("<  >", "seek −/+ 10 s"),
    ("s  r", "shuffle · repeat (off → all → one)"),
    ("N", "new playlist"),
    ("e  D", "(library) rename · delete playlist"),
    ("a", "(track) add to playlist"),
    ("x", "(track) remove from playlist / Liked songs"),
    ("L", "(track) save to Liked songs"),
    ("R", "refresh everything"),
    ("q  Ctrl+C", "quit"),
];

fn draw_popup(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let dim = Style::new().fg(Color::DarkGray);
    match &mut app.popup {
        Popup::Closed => {}
        Popup::Help => {
            let lines: Vec<Line> = HELP
                .iter()
                .map(|(k, d)| {
                    Line::from(vec![
                        Span::styled(format!("{k:>26}  "), Style::new().fg(ACCENT).bold()),
                        Span::raw(*d),
                    ])
                })
                .collect();
            let r = centered(area, 86, lines.len() as u16 + 2);
            f.render_widget(Clear, r);
            f.render_widget(Paragraph::new(lines).block(pane(" Keys — any key closes ".into(), true)), r);
        }
        Popup::Input { purpose, text } => {
            let r = centered(area, 64, 3);
            f.render_widget(Clear, r);
            let p = Paragraph::new(Line::from(vec![Span::raw(text.clone()), Span::styled("█", Style::new().fg(ACCENT))]))
                .block(pane(purpose.title(), true));
            f.render_widget(p, r);
        }
        Popup::Confirm { prompt, .. } => {
            let r = centered(area, 64, 5);
            f.render_widget(Clear, r);
            let p = Paragraph::new(vec![
                Line::from(prompt.clone()),
                Line::from(""),
                Line::from(Span::styled("y = yes   n / Esc = no", dim)),
            ])
            .wrap(Wrap { trim: true })
            .block(pane(" Confirm ".into(), true));
            f.render_widget(p, r);
        }
        Popup::PickPlaylist { label, choices, state, .. } => {
            let h = (choices.len() as u16 + 2).clamp(3, area.height.saturating_sub(4));
            let r = centered(area, 60, h);
            f.render_widget(Clear, r);
            let items: Vec<ListItem> = choices.iter().map(|p| ListItem::new(p.name.clone())).collect();
            let list = List::new(items)
                .block(pane(format!(" Add “{label}” to… "), true))
                .highlight_style(Style::new().add_modifier(Modifier::REVERSED))
                .highlight_symbol("▌");
            f.render_stateful_widget(list, r, state);
        }
        Popup::Devices { list, state, loading } => {
            let mut items: Vec<ListItem> = Vec::new();
            for d in list.iter() {
                let alias = app.cfg.alias_for(d.id.as_deref(), &d.name);
                let mut spans = vec![
                    Span::styled(if d.is_active { "● " } else { "  " }, Style::new().fg(ACCENT)),
                    Span::styled(alias.unwrap_or(&d.name).to_string(), Style::new().bold()),
                ];
                let mut meta = Vec::new();
                if alias.is_some() {
                    meta.push(crate::app::short(&d.name));
                }
                if !d.kind.is_empty() {
                    meta.push(d.kind.to_lowercase());
                }
                if let Some(v) = d.volume {
                    meta.push(format!("vol {v}%"));
                }
                if d.is_restricted || d.id.is_none() {
                    meta.push("can't be controlled".into());
                }
                spans.push(Span::styled(format!("  {}", meta.join(" · ")), dim));
                items.push(ListItem::new(Line::from(spans)));
            }
            let title = if *loading {
                " Speakers · looking… ".to_string()
            } else if list.is_empty() {
                " Speakers · none found ".to_string()
            } else {
                " Speakers ".to_string()
            };
            let hint = Line::from(Span::styled(" Enter play here · e rename · r refresh · Esc close ", dim));
            let h = (items.len().max(1) as u16 + 2).clamp(4, area.height.saturating_sub(4));
            let r = centered(area, 72, h);
            f.render_widget(Clear, r);
            let block = pane(title, true).title_bottom(hint);
            if items.is_empty() && !*loading {
                let p = Paragraph::new("No speakers visible. Turn one on, or open Spotify on it once, then press r.")
                    .wrap(Wrap { trim: true })
                    .style(dim)
                    .block(block);
                f.render_widget(p, r);
            } else {
                let w = List::new(items)
                    .block(block)
                    .highlight_style(Style::new().add_modifier(Modifier::REVERSED))
                    .highlight_symbol("▌");
                f.render_stateful_widget(w, r, state);
            }
        }
    }
}
