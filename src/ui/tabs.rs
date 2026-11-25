//! Contents of the player box tabs.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Cell, Paragraph, Row, Table, Wrap};
use ratatui_image::{Resize, StatefulImage};

use super::progress_spans;
use crate::app::{App, AuthInput, Hit, LibSection, ListView, LyricsState};
use crate::keymap::Action;
use crate::lyrics::{Lyrics, active_line};
use crate::model::{Item, format_time};
use crate::player::queue::Repeat;
use crate::theme::Theme;
use crate::ytm::SearchFilter;

const SPINNER: [&str; 8] = ["⣾", "⣽", "⣻", "⢿", "⡿", "⣟", "⣯", "⣷"];

fn spinner() -> &'static str {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    SPINNER[(ms / 100) as usize % SPINNER.len()]
}

fn centered_msg(f: &mut Frame, area: Rect, theme: &Theme, lines: Vec<Line<'static>>) {
    let h = lines.len() as u16;
    let y = area.y + area.height.saturating_sub(h) / 2;
    f.render_widget(
        Paragraph::new(lines)
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true })
            .style(Style::new().fg(theme.inactive_fg)),
        Rect {
            y,
            height: h.min(area.height),
            ..area
        },
    );
}

// ───────────────────────────── now playing ─────────────────────────────

pub fn now_playing(f: &mut Frame, app: &mut App, area: Rect) {
    let t = app.theme.clone();
    let Some(track) = app.current().cloned() else {
        let k = app.keymap.hint(Action::TabSearch);
        centered_msg(
            f,
            area,
            &t,
            vec![
                Line::from("♪ nothing playing"),
                Line::from(""),
                Line::from(format!("press {k} to search YouTube Music")),
            ],
        );
        return;
    };

    // Text block below the art: title, artist, album, gap, progress, time,
    // status, gap, up next.
    let text_rows = 9u16;
    let art_rows = area.height.saturating_sub(text_rows + 1);
    // Terminal cells are ~2:1, so a square image is twice as wide as tall.
    let art_h = art_rows.min(area.width / 2);
    let mut y = area.y;
    if app.cfg.album_art && art_h >= 4 {
        let art_w = (art_h * 2).min(area.width);
        let art_area = Rect::new(area.x + (area.width - art_w) / 2, area.y, art_w, art_h);
        let mut drew = false;
        if let Some(proto) = app.art.as_mut()
            && !app.art_loading
            && let Some(size) = proto.size_for(Resize::Scale(None), art_area.into())
        {
            // Centre the fitted image inside the art area.
            let ax = art_area.x + (art_area.width.saturating_sub(size.width)) / 2;
            let ay = art_area.y + (art_area.height.saturating_sub(size.height)) / 2;
            let r = Rect::new(ax, ay, size.width.min(art_area.width), size.height.min(art_area.height));
            f.render_stateful_widget(StatefulImage::new().resize(Resize::Scale(None)), r, proto);
            drew = true;
        }
        if !drew {
            let block = Block::bordered()
                .border_type(ratatui::widgets::BorderType::Rounded)
                .border_style(Style::new().fg(t.div_line));
            let inner = block.inner(art_area);
            f.render_widget(block, art_area);
            let icon = if app.art_loading { spinner() } else { "♪" };
            f.render_widget(
                Paragraph::new(icon)
                    .alignment(Alignment::Center)
                    .style(Style::new().fg(t.inactive_fg)),
                Rect {
                    y: inner.y + inner.height / 2,
                    height: 1,
                    ..inner
                },
            );
        }
        y += art_h + 1;
    } else {
        // No art: vertically centre the text block.
        y += area.height.saturating_sub(text_rows) / 2;
    }

    let line = |y: u16| Rect::new(area.x, y, area.width, 1);
    let bottom = area.y + area.height;
    let put = |f: &mut Frame, y: &mut u16, p: Paragraph| {
        if *y < bottom {
            f.render_widget(p.alignment(Alignment::Center), line(*y));
        }
        *y += 1;
    };
    let liked = if crate::app::is_liked(&app.likes, &track) {
        " ♥"
    } else {
        ""
    };
    put(
        f,
        &mut y,
        Paragraph::new(Line::from(vec![
            Span::styled(
                track.title.clone(),
                Style::new().fg(t.title).add_modifier(Modifier::BOLD),
            ),
            Span::styled(liked, Style::new().fg(t.hi_fg)),
        ])),
    );
    put(
        f,
        &mut y,
        Paragraph::new(Span::styled(track.artists.clone(), Style::new().fg(t.main_fg))),
    );
    put(
        f,
        &mut y,
        Paragraph::new(Span::styled(
            track.album.clone().unwrap_or_default(),
            Style::new().fg(t.graph_text),
        )),
    );
    y += 1;

    // Progress bar (clickable).
    if y < bottom {
        let w = area.width.saturating_sub(4);
        if w >= 2 {
            let frac = if app.player.duration > 0.0 {
                app.player.time / app.player.duration
            } else {
                0.0
            };
            let r = Rect::new(area.x + 2, y, w, 1);
            f.render_widget(Paragraph::new(Line::from(progress_spans(&t, w, frac))), r);
            app.hits.add(r, Hit::Progress);
        }
    }
    y += 1;
    if y < bottom {
        let r = Rect::new(area.x + 2, y, area.width.saturating_sub(4), 1);
        let status = if app.player.error.is_some() {
            "mpv unavailable".into()
        } else if app.player.buffering || (app.player.idle && app.current().is_some() && !app.player.paused) {
            format!("{} loading", spinner())
        } else if app.player.paused {
            "paused".into()
        } else {
            String::new()
        };
        f.render_widget(
            Paragraph::new(format_time(app.player.time)).style(Style::new().fg(t.graph_text)),
            r,
        );
        f.render_widget(
            Paragraph::new(status)
                .alignment(Alignment::Center)
                .style(Style::new().fg(t.inactive_fg)),
            r,
        );
        f.render_widget(
            Paragraph::new(format_time(app.player.duration))
                .alignment(Alignment::Right)
                .style(Style::new().fg(t.graph_text)),
            r,
        );
    }
    y += 1;
    if y < bottom {
        let dim = Style::new().fg(t.inactive_fg);
        let lit = Style::new().fg(t.accent);
        let vol = if app.player.muted {
            "muted".into()
        } else {
            format!("vol {:.0}%", app.player.volume)
        };
        let pos = app
            .queue
            .current
            .map(|c| format!("{}/{}", c + 1, app.queue.len()))
            .unwrap_or_default();
        let line_spans = vec![
            Span::styled(vol, Style::new().fg(t.graph_text)),
            Span::raw("   "),
            Span::styled("⇄ shuffle", if app.queue.shuffle { lit } else { dim }),
            Span::raw("   "),
            Span::styled(
                format!("↻ {}", app.queue.repeat.label()),
                if app.queue.repeat != Repeat::Off { lit } else { dim },
            ),
            Span::raw("   "),
            Span::styled(pos, Style::new().fg(t.graph_text)),
        ];
        f.render_widget(
            Paragraph::new(Line::from(line_spans)).alignment(Alignment::Center),
            line(y),
        );
    }
    y += 2;
    if y < bottom
        && let Some(next) = up_next(app)
    {
        let label = "up next  ";
        let room = (area.width as usize).saturating_sub(label.chars().count() + 4);
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(label, Style::new().fg(t.inactive_fg)),
                Span::styled(ellipsize(&next, room), Style::new().fg(t.graph_text)),
            ]))
            .alignment(Alignment::Center),
            line(y),
        );
    }
}

/// What plays after the current song, for the "up next" line. None when the
/// current song repeats.
pub fn up_next(app: &App) -> Option<String> {
    if app.queue.repeat == Repeat::One {
        return None;
    }
    // A one-song queue on repeat-all "continues" with itself.
    let next = app.queue.next_index().filter(|&i| Some(i) != app.queue.current);
    if next.is_none() && app.queue.repeat == Repeat::All {
        return None;
    }
    match next.and_then(|i| app.queue.get(i)) {
        Some(t) if t.artists.is_empty() => Some(t.title.clone()),
        Some(t) => Some(format!("{} · {}", t.title, t.artists)),
        None if app.cfg.autoplay => Some("similar songs (autoplay)".into()),
        None => Some("nothing, end of the queue".into()),
    }
}

/// Cut `s` to `max` terminal columns, ending in "…" when shortened.
pub(crate) fn ellipsize(s: &str, max: usize) -> String {
    if Span::raw(s).width() <= max {
        return s.to_string();
    }
    let mut out = String::new();
    let mut w = 0;
    for c in s.chars() {
        let cw = Span::raw(c.to_string()).width();
        if w + cw + 1 > max {
            break;
        }
        out.push(c);
        w += cw;
    }
    out.push('…');
    out
}

// ───────────────────────────── lists ─────────────────────────────

fn table_style(t: &Theme) -> Style {
    Style::new()
        .bg(t.selected_bg)
        .fg(t.selected_fg)
        .add_modifier(Modifier::BOLD)
}

pub fn queue(f: &mut Frame, app: &mut App, area: Rect) {
    let t = app.theme.clone();
    if app.queue.is_empty() {
        centered_msg(
            f,
            area,
            &t,
            vec![
                Line::from("queue is empty"),
                Line::from(""),
                Line::from(format!(
                    "{} search · {} add · {} play next",
                    app.keymap.hint(Action::TabSearch),
                    app.keymap.hint(Action::Append),
                    app.keymap.hint(Action::PlayNext)
                )),
            ],
        );
        return;
    }
    let cur = app.queue.current;
    let rows: Vec<Row> = app
        .queue
        .entries
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let tr = &e.track;
            let is_cur = Some(i) == cur;
            let played = cur.is_some_and(|c| i < c);
            let style = if is_cur {
                Style::new().fg(t.accent).add_modifier(Modifier::BOLD)
            } else if played {
                Style::new().fg(t.inactive_fg)
            } else {
                Style::new().fg(t.main_fg)
            };
            let marker = if is_cur {
                if app.player.paused { "⏸" } else { "▶" }
            } else {
                ""
            };
            let heart = if crate::app::is_liked(&app.likes, tr) {
                "♥"
            } else {
                ""
            };
            Row::new(vec![
                Cell::from(marker),
                Cell::from(format!("{}", i + 1)).style(Style::new().fg(t.graph_text)),
                Cell::from(heart).style(Style::new().fg(t.hi_fg)),
                Cell::from(tr.title.clone()),
                Cell::from(tr.artists.clone()).style(if played { style } else { Style::new().fg(t.graph_text) }),
                Cell::from(tr.duration.map(format_time).unwrap_or_default()),
            ])
            .style(style)
        })
        .collect();
    let num_w = (app.queue.len().to_string().len() as u16).max(2);
    let header = Row::new(vec!["", "#", "", "title", "artist", "time"]).style(Style::new().fg(t.hi_fg));
    let table = Table::new(
        rows,
        [
            Constraint::Length(1),
            Constraint::Length(num_w),
            Constraint::Length(1),
            Constraint::Fill(5),
            Constraint::Fill(3),
            Constraint::Length(7),
        ],
    )
    .header(header)
    .column_spacing(1)
    .row_highlight_style(table_style(&t));
    if app.queue_state.selected().is_none() {
        app.queue_state.select(cur);
    }
    f.render_stateful_widget(table, area, &mut app.queue_state);
    let list_area = Rect {
        y: area.y + 1,
        height: area.height.saturating_sub(1),
        ..area
    };
    app.hits.list = Some((list_area, app.queue_state.offset()));
    app.hits.add(list_area, Hit::List);
}

fn item_rows<'a>(
    t: &Theme,
    items: &'a [Item],
    playing: Option<&str>,
    likes: &std::collections::HashMap<String, bool>,
) -> Vec<Row<'a>> {
    items
        .iter()
        .map(|item| match item {
            Item::Track(tr) => {
                let is_playing = playing == Some(tr.video_id.as_str());
                let style = if is_playing {
                    Style::new().fg(t.accent)
                } else {
                    Style::new().fg(t.main_fg)
                };
                Row::new(vec![
                    Cell::from(if is_playing {
                        "▶"
                    } else if crate::app::is_liked(likes, tr) {
                        "♥"
                    } else {
                        "♪"
                    })
                    .style(Style::new().fg(t.hi_fg)),
                    Cell::from(tr.title.as_str()),
                    Cell::from(tr.artists.as_str()).style(Style::new().fg(t.graph_text)),
                    Cell::from(tr.duration.map(format_time).unwrap_or_default()),
                ])
                .style(style)
            }
            Item::Album(a) => Row::new(vec![
                Cell::from("◉").style(Style::new().fg(t.hi_fg)),
                Cell::from(a.title.as_str()),
                Cell::from(a.subtitle.as_str()).style(Style::new().fg(t.graph_text)),
                Cell::from("album").style(Style::new().fg(t.inactive_fg)),
            ]),
            Item::Artist(a) => Row::new(vec![
                Cell::from("☺").style(Style::new().fg(t.hi_fg)),
                Cell::from(a.name.as_str()),
                Cell::from(a.subtitle.as_str()).style(Style::new().fg(t.graph_text)),
                Cell::from("artist").style(Style::new().fg(t.inactive_fg)),
            ]),
            Item::Playlist(p) => Row::new(vec![
                Cell::from("≡").style(Style::new().fg(t.hi_fg)),
                Cell::from(p.title.as_str()),
                Cell::from(p.subtitle.as_str()).style(Style::new().fg(t.graph_text)),
                Cell::from("list").style(Style::new().fg(t.inactive_fg)),
            ]),
            Item::Header(h) => Row::new(vec![
                Cell::from(""),
                Cell::from(h.as_str()).style(
                    Style::new()
                        .fg(t.title)
                        .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
                ),
            ]),
        })
        .collect()
}

/// Breadcrumb + table for a stack of list views.
fn browse(f: &mut Frame, app: &mut App, area: Rect, library: bool) {
    let t = app.theme.clone();
    let playing = app.current().map(|tr| tr.video_id.clone());
    let back = app.keymap.hint(Action::Menu);
    let likes = &app.likes;
    let stack: &mut Vec<ListView> = if library {
        &mut app.library.stack
    } else {
        &mut app.search.stack
    };
    if stack.is_empty() {
        return;
    }
    let depth = stack.len();
    let lv = stack.last_mut().unwrap();

    let [crumb, body] = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);
    let mut spans = vec![Span::styled(
        if lv.title.is_empty() {
            "…".to_string()
        } else {
            lv.title.clone()
        },
        Style::new().fg(t.title).add_modifier(Modifier::BOLD),
    )];
    if lv.loading {
        spans.push(Span::styled(format!("  {}", spinner()), Style::new().fg(t.accent)));
    } else if !lv.items.is_empty() {
        let n = lv.items.iter().filter(|i| !matches!(i, Item::Header(_))).count();
        spans.push(Span::styled(format!("  {n} items"), Style::new().fg(t.inactive_fg)));
    }
    if depth > 1 {
        spans.push(Span::styled(format!("  ‹{back} back›"), Style::new().fg(t.inactive_fg)));
    }
    if library {
        spans.push(Span::styled("  ‹u account›", Style::new().fg(t.inactive_fg)));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), crumb);

    if let Some(err) = &lv.error {
        // Wrap here so the full message shows (centered_msg sizes by line count).
        let width = body.width.saturating_sub(4).clamp(20, 90) as usize;
        let lines = crate::ui::wrap_text(err, width)
            .into_iter()
            .map(|l| Line::from(Span::styled(l, Style::new().fg(t.error))))
            .collect();
        centered_msg(f, body, &t, lines);
        return;
    }
    if lv.loading && lv.items.is_empty() {
        centered_msg(f, body, &t, vec![Line::from(format!("{} loading…", spinner()))]);
        return;
    }
    if library && lv.items.is_empty() {
        centered_msg(
            f,
            body,
            &t,
            vec![
                Line::from("nothing here yet"),
                Line::from(""),
                Line::from("on a YouTube Brand Account? press u to switch account"),
            ],
        );
        return;
    }
    let rows = item_rows(&t, &lv.items, playing.as_deref(), likes);
    let table = Table::new(
        rows,
        [
            Constraint::Length(1),
            Constraint::Fill(5),
            Constraint::Fill(3),
            Constraint::Length(7),
        ],
    )
    .column_spacing(1)
    .row_highlight_style(table_style(&t));
    f.render_stateful_widget(table, body, &mut lv.state);
    let offset = lv.state.offset();
    app.hits.list = Some((body, offset));
    app.hits.add(body, Hit::List);
}

fn chips<T: Copy + PartialEq>(
    f: &mut Frame,
    app: &mut App,
    area: Rect,
    items: &[(T, &str)],
    active: T,
    hit: impl Fn(T) -> Hit,
) {
    let t = app.theme.clone();
    let mut spans = Vec::new();
    let mut x = area.x;
    for (val, label) in items {
        let text = format!(" {label} ");
        let w = Span::raw(text.as_str()).width() as u16;
        let style = if *val == active {
            Style::new()
                .bg(t.selected_bg)
                .fg(t.selected_fg)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(t.graph_text)
        };
        if x + w <= area.x + area.width {
            app.hits.add(Rect::new(x, area.y, w, 1), hit(*val));
        }
        x += w + 1;
        spans.push(Span::styled(text, style));
        spans.push(Span::raw(" "));
    }
    let hint = format!(
        "{}/{}",
        app.keymap.hint(Action::SubPrev),
        app.keymap.hint(Action::SubNext)
    );
    spans.push(Span::styled(hint, Style::new().fg(t.inactive_fg)));
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

pub fn search(f: &mut Frame, app: &mut App, area: Rect) {
    let t = app.theme.clone();
    let [input, filters, gap, body] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .areas(area);

    let prompt_style = if app.search.editing {
        Style::new().fg(t.hi_fg)
    } else {
        Style::new().fg(t.graph_text)
    };
    let text = if app.search.input.is_empty() && !app.search.editing {
        Span::styled(
            format!("press {} to search…", app.keymap.hint(Action::TabSearch)),
            Style::new().fg(t.inactive_fg),
        )
    } else {
        Span::styled(app.search.input.clone(), Style::new().fg(t.title))
    };
    let mut line = vec![text];
    // Inline completion from the top suggestion, dimmed (→ accepts it).
    if let Some(ghost) = app.search.ghost() {
        line.push(Span::styled(ghost.to_string(), Style::new().fg(t.inactive_fg)));
    }
    let prompt_w = 2u16.min(input.width);
    f.render_widget(
        Paragraph::new(Span::styled("⌕ ", prompt_style)),
        Rect {
            width: prompt_w,
            ..input
        },
    );
    let field = Rect {
        x: input.x + prompt_w,
        width: input.width.saturating_sub(prompt_w),
        ..input
    };
    let mut scroll_x = 0u16;
    if app.search.editing && field.width > 0 {
        let before: String = app.search.input.chars().take(app.search.cursor).collect();
        let cursor_w = Span::raw(before.as_str()).width() as u16;
        scroll_x = cursor_w.saturating_sub(field.width.saturating_sub(1));
        let cx = field.x + cursor_w.saturating_sub(scroll_x);
        f.set_cursor_position((cx.min(field.x + field.width.saturating_sub(1)), field.y));
    }
    f.render_widget(Paragraph::new(Line::from(line)).scroll((0, scroll_x)), field);
    // Divider under the input, btop style.
    f.render_widget(
        Paragraph::new("─".repeat(gap.width as usize)).style(Style::new().fg(t.div_line)),
        gap,
    );

    let items: Vec<(SearchFilter, &str)> = SearchFilter::ALL.iter().map(|f| (*f, f.label())).collect();
    let active = app.search.filter;
    chips(f, app, filters, &items, active, Hit::SearchFilter);

    if app.search.stack.is_empty() {
        centered_msg(
            f,
            body,
            &t,
            vec![
                Line::from("type and press enter"),
                Line::from("tab switches the filter while typing"),
            ],
        );
    } else {
        browse(f, app, body, false);
    }
    suggestions_dropdown(
        f,
        app,
        Rect {
            y: input.y + 1,
            height: area.height.saturating_sub(1),
            ..area
        },
    );
}

/// Autocomplete list under the search box, drawn over the results.
fn suggestions_dropdown(f: &mut Frame, app: &mut App, below: Rect) {
    let t = app.theme.clone();
    let list = app.search.visible_suggestions().to_vec();
    if list.is_empty() || below.height < 3 {
        return;
    }
    let rows = (list.len() as u16).min(below.height - 2);
    let r = Rect {
        height: rows + 2,
        ..below
    };
    f.render_widget(ratatui::widgets::Clear, r);
    let block = Block::bordered()
        .border_type(ratatui::widgets::BorderType::Rounded)
        .border_style(Style::new().fg(t.hi_fg))
        .style(Style::new().bg(t.main_bg))
        .title_bottom(
            Line::from(Span::styled(
                "┐↑↓ select · → complete · enter search┌",
                Style::new().fg(t.inactive_fg),
            ))
            .alignment(Alignment::Right),
        );
    let inner = block.inner(r);
    f.render_widget(block, r);
    let typed = app.search.input.trim();
    for (i, sugg) in list.iter().take(rows as usize).enumerate() {
        let y = inner.y + i as u16;
        let row = Rect { y, height: 1, ..inner };
        let selected = app.search.suggest_sel == Some(i);
        let base = if selected {
            Style::new().bg(t.selected_bg).fg(t.selected_fg)
        } else {
            Style::new().fg(t.main_fg)
        };
        // Like YTM: the typed part plain, the completion bold.
        let split = crate::app::case_insensitive_prefix_len(&sugg.text, typed).unwrap_or(0);
        let (head, tail) = sugg.text.split_at(split);
        let icon = if sugg.history { "↺ " } else { "⌕ " };
        let spans = vec![
            Span::styled(
                format!(" {icon}"),
                base.fg(if selected { t.selected_fg } else { t.hi_fg }),
            ),
            Span::styled(head.to_string(), base),
            Span::styled(tail.to_string(), base.add_modifier(Modifier::BOLD)),
        ];
        f.render_widget(Paragraph::new(Line::from(spans)).style(base), row);
        app.hits.add(row, Hit::Suggestion(i));
    }
}

pub fn library(f: &mut Frame, app: &mut App, area: Rect) {
    let t = app.theme.clone();
    let [sections, gap, body] =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1), Constraint::Min(0)]).areas(area);
    let items: Vec<(LibSection, &str)> = LibSection::ALL.iter().map(|s| (*s, s.label())).collect();
    let active = app.library.section;
    chips(f, app, sections, &items, active, Hit::LibSection);
    f.render_widget(
        Paragraph::new("─".repeat(gap.width as usize)).style(Style::new().fg(t.div_line)),
        gap,
    );
    if !app.logged_in {
        library_sign_in(f, app, body);
        return;
    }
    browse(f, app, body, true);
}

/// Interactive sign-in view shown in the Library tab when signed out.
fn library_sign_in(f: &mut Frame, app: &mut App, area: Rect) {
    let t = app.theme.clone();
    if area.width < 12 || area.height < 4 {
        return;
    }
    let text_w = area.width.saturating_sub(4).clamp(12, 54) as usize;

    // Waiting for the user to finish signing in at https://music.youtube.com.
    if let Some(w) = app.library.auth.waiting.clone() {
        let desc = if w.browser_opened {
            format!("Sign in with your Google account in {} at:", w.label)
        } else {
            format!("Open this link in {} and sign in with Google:", w.label)
        };
        let desc_lines = crate::ui::wrap_text(&desc, text_w);
        let url = crate::auth::YTM_SIGN_IN_URL;
        let status = if w.checking {
            format!("{} checking {}…", spinner(), w.label)
        } else if w.started.elapsed() > crate::app::AUTH_POLL_TIMEOUT {
            "auto-check paused — press enter to check now".to_string()
        } else {
            format!("{} waiting for {}…", spinner(), w.label)
        };
        // Title, blank, desc, blank, url, blank, status, blank, primary buttons, cancel.
        let total_h = (1 + 1 + desc_lines.len() + 1 + 1 + 1 + 1 + 1 + 1 + 1) as u16;
        let mut y = area.y + area.height.saturating_sub(total_h) / 2;
        let bottom = area.y + area.height;

        if y < bottom {
            f.render_widget(
                Paragraph::new(Span::styled(
                    "Sign in to YouTube Music",
                    Style::new().fg(t.title).add_modifier(Modifier::BOLD),
                ))
                .alignment(Alignment::Center),
                Rect::new(area.x, y, area.width, 1),
            );
            y += 2;
        }
        for l in desc_lines {
            if y >= bottom {
                break;
            }
            f.render_widget(
                Paragraph::new(l)
                    .alignment(Alignment::Center)
                    .style(Style::new().fg(t.inactive_fg)),
                Rect::new(area.x, y, area.width, 1),
            );
            y += 1;
        }
        y += 1;
        if y < bottom {
            let url_w = (Span::raw(url).width() as u16).min(area.width);
            let ux = area.x + area.width.saturating_sub(url_w) / 2;
            let url_rect = Rect::new(ux, y, url_w, 1);
            f.render_widget(
                Paragraph::new(Span::styled(
                    url,
                    Style::new().fg(t.accent).add_modifier(Modifier::UNDERLINED),
                ))
                .alignment(Alignment::Center),
                url_rect,
            );
            app.hits.add(url_rect, Hit::AuthOpenBrowser);
            y += 2;
        }
        if y < bottom {
            f.render_widget(
                Paragraph::new(Span::styled(status, Style::new().fg(t.main_fg))).alignment(Alignment::Center),
                Rect::new(area.x, y, area.width, 1),
            );
            y += 2;
        }
        if y < bottom {
            let check_lbl = " Check now (enter) ";
            let open_lbl = " Open link (o) ";
            let check_w = Span::raw(check_lbl).width() as u16;
            let open_w = Span::raw(open_lbl).width() as u16;
            let gap = 2u16;
            if check_w + gap + open_w <= area.width {
                let x0 = area.x + (area.width - (check_w + gap + open_w)) / 2;
                let check_rect = Rect::new(x0, y, check_w, 1);
                let open_rect = Rect::new(x0 + check_w + gap, y, open_w, 1);
                f.render_widget(
                    Paragraph::new(Span::styled(
                        check_lbl,
                        Style::new()
                            .bg(t.selected_bg)
                            .fg(t.selected_fg)
                            .add_modifier(Modifier::BOLD),
                    )),
                    check_rect,
                );
                f.render_widget(
                    Paragraph::new(Span::styled(open_lbl, Style::new().fg(t.graph_text))),
                    open_rect,
                );
                app.hits.add(check_rect, Hit::AuthCheckNow);
                app.hits.add(open_rect, Hit::AuthOpenBrowser);
            } else {
                let x0 = area.x + area.width.saturating_sub(check_w) / 2;
                let check_rect = Rect::new(x0, y, check_w.min(area.width), 1);
                f.render_widget(
                    Paragraph::new(Span::styled(
                        check_lbl,
                        Style::new()
                            .bg(t.selected_bg)
                            .fg(t.selected_fg)
                            .add_modifier(Modifier::BOLD),
                    )),
                    check_rect,
                );
                app.hits.add(check_rect, Hit::AuthCheckNow);
            }
            y += 1;
        }
        if y < bottom {
            let cancel_lbl = " Cancel (esc) ";
            let cw = Span::raw(cancel_lbl).width() as u16;
            let cx = area.x + area.width.saturating_sub(cw) / 2;
            let cancel_rect = Rect::new(cx, y, cw.min(area.width), 1);
            f.render_widget(
                Paragraph::new(Span::styled(cancel_lbl, Style::new().fg(t.inactive_fg))),
                cancel_rect,
            );
            app.hits.add(cancel_rect, Hit::AuthCancel);
        }
        return;
    }

    // Spinner while checking browsers or saving a cookie.
    if let Some(status) = app.library.auth.busy.clone() {
        let mut lines = vec![
            Line::from(Span::styled(
                format!("{} {status}…", spinner()),
                Style::new().fg(t.title).add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
        ];
        for l in crate::ui::wrap_text("if your desktop keyring asks to unlock, approve it", text_w) {
            lines.push(Line::from(l));
        }
        centered_msg(f, area, &t, lines);
        return;
    }

    // Inline text input for custom browser profile or manual cookie paste.
    if let Some(input) = app.library.auth.input.clone() {
        let (title, hint, shown) = match &input {
            AuthInput::CustomBrowser(val) => (
                "Sign in with browser profile",
                "Enter a browser or profile (e.g. chrome:Profile 2 or firefox:work):",
                val.clone(),
            ),
            AuthInput::PasteCookie(val) => {
                let n = val.chars().count();
                let masked = if n == 0 {
                    String::new()
                } else {
                    format!("{} ({n} chars)", "•".repeat(n.min(20)))
                };
                (
                    "Paste YouTube Music cookie",
                    "In your browser (signed in to music.youtube.com): DevTools → Network → any request → copy the Cookie header and paste here:",
                    masked,
                )
            }
        };
        let hint_lines = crate::ui::wrap_text(hint, text_w);
        let box_w = area.width.saturating_sub(4).clamp(16, 48);
        let total_h = (1 + 1 + hint_lines.len() + 1 + 3 + 1 + 1) as u16;
        let mut y = area.y + area.height.saturating_sub(total_h) / 2;
        let bottom = area.y + area.height;

        if y < bottom {
            f.render_widget(
                Paragraph::new(Span::styled(
                    title,
                    Style::new().fg(t.title).add_modifier(Modifier::BOLD),
                ))
                .alignment(Alignment::Center),
                Rect::new(area.x, y, area.width, 1),
            );
            y += 2;
        }
        for l in hint_lines {
            if y >= bottom {
                break;
            }
            f.render_widget(
                Paragraph::new(l)
                    .alignment(Alignment::Center)
                    .style(Style::new().fg(t.inactive_fg)),
                Rect::new(area.x, y, area.width, 1),
            );
            y += 1;
        }
        y += 1;
        if y + 3 <= bottom {
            let bx = area.x + (area.width.saturating_sub(box_w)) / 2;
            let field_box = Rect::new(bx, y, box_w, 3);
            let block = Block::bordered()
                .border_type(ratatui::widgets::BorderType::Rounded)
                .border_style(Style::new().fg(t.hi_fg));
            let inner = block.inner(field_box);
            f.render_widget(block, field_box);
            let inner_w = inner.width.saturating_sub(2) as usize;
            let visible: String = if shown.chars().count() + 1 > inner_w && inner_w > 1 {
                let skip = shown.chars().count() + 1 - inner_w;
                shown.chars().skip(skip).collect()
            } else {
                shown
            };
            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(visible, Style::new().fg(t.main_fg)),
                    Span::styled("█", Style::new().fg(t.hi_fg)),
                ])),
                Rect {
                    x: inner.x + 1,
                    width: inner.width.saturating_sub(2),
                    ..inner
                },
            );
            y += 4;
        }
        if y < bottom {
            f.render_widget(
                Paragraph::new("enter sign in · esc back")
                    .alignment(Alignment::Center)
                    .style(Style::new().fg(t.inactive_fg)),
                Rect::new(area.x, y, area.width, 1),
            );
        }
        return;
    }

    // Default signed-out view: show the sign-in link right away + every detected
    // browser profile by name (so multi-profile Brave/Chrome/Firefox works in 1 click).
    app.library.auth.ensure_profiles();
    let url = crate::auth::YTM_SIGN_IN_URL;
    let profiles = app.library.auth.profiles.clone();
    let selected = app.library.auth.selected.min(profiles.len().saturating_sub(1));
    // Reserve rows for header + link + prompt + footer hints; scroll list if many profiles.
    let chrome_h = 9u16;
    let max_visible = area.height.saturating_sub(chrome_h).clamp(1, 8) as usize;
    let visible_n = profiles.len().min(max_visible);
    let scroll = if selected >= visible_n {
        selected + 1 - visible_n
    } else {
        0
    };
    let total_h = chrome_h + visible_n as u16;
    let mut y = area.y + area.height.saturating_sub(total_h) / 2;
    let bottom = area.y + area.height;

    if y < bottom {
        f.render_widget(
            Paragraph::new(Span::styled(
                "Sign in to YouTube Music",
                Style::new().fg(t.title).add_modifier(Modifier::BOLD),
            ))
            .alignment(Alignment::Center),
            Rect::new(area.x, y, area.width, 1),
        );
        y += 2;
    }
    if y < bottom {
        f.render_widget(
            Paragraph::new("1. Sign in with Google at:")
                .alignment(Alignment::Center)
                .style(Style::new().fg(t.inactive_fg)),
            Rect::new(area.x, y, area.width, 1),
        );
        y += 1;
    }
    if y < bottom {
        let url_w = (Span::raw(url).width() as u16).min(area.width);
        let ux = area.x + area.width.saturating_sub(url_w) / 2;
        let url_rect = Rect::new(ux, y, url_w, 1);
        f.render_widget(
            Paragraph::new(Span::styled(
                url,
                Style::new().fg(t.accent).add_modifier(Modifier::UNDERLINED),
            ))
            .alignment(Alignment::Center),
            url_rect,
        );
        app.hits.add(url_rect, Hit::AuthOpenBrowser);
        y += 2;
    }
    if y < bottom {
        f.render_widget(
            Paragraph::new("2. Pick your browser profile:")
                .alignment(Alignment::Center)
                .style(Style::new().fg(t.inactive_fg)),
            Rect::new(area.x, y, area.width, 1),
        );
        y += 1;
    }

    let list_w = area.width.saturating_sub(4).clamp(16, 40);
    let list_x = area.x + area.width.saturating_sub(list_w) / 2;
    for (i, prof) in profiles.iter().enumerate().skip(scroll).take(visible_n) {
        if y >= bottom {
            break;
        }
        let is_sel = i == selected;
        let prefix = if is_sel { " ▸ " } else { "   " };
        let tag = if prof.default { "default" } else { "" };
        let style = if is_sel {
            Style::new()
                .bg(t.selected_bg)
                .fg(t.selected_fg)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(t.main_fg)
        };
        let row = Rect::new(list_x, y, list_w, 1);
        let tag_space = if tag.is_empty() { 0 } else { tag.len() + 2 };
        let max_lbl = (list_w as usize).saturating_sub(3 + tag_space);
        let lbl = ellipsize(&prof.label, max_lbl);
        let left_text = format!("{prefix}{lbl}");
        let left_w = Span::raw(left_text.as_str()).width();
        let tag_w = Span::raw(tag).width();
        let pad = (list_w as usize).saturating_sub(left_w + tag_w + usize::from(!tag.is_empty()));
        let mut spans = vec![Span::styled(left_text, style)];
        if !tag.is_empty() && pad >= 1 {
            spans.push(Span::styled(" ".repeat(pad), style));
            spans.push(Span::styled(
                format!("{tag} "),
                if is_sel { style } else { Style::new().fg(t.inactive_fg) },
            ));
        } else if pad > 0 {
            spans.push(Span::styled(" ".repeat(pad), style));
        }
        f.render_widget(Paragraph::new(Line::from(spans)).style(style), row);
        app.hits.add(row, Hit::AuthProfile(i));
        y += 1;
    }

    y += 1;
    if y < bottom {
        f.render_widget(
            Paragraph::new("↑↓ select · enter sign in · o open link")
                .alignment(Alignment::Center)
                .style(Style::new().fg(t.inactive_fg)),
            Rect::new(area.x, y, area.width, 1),
        );
        y += 1;
    }
    if y < bottom {
        let prof_lbl = "other profile (p)";
        let sep = "  ·  ";
        let paste_lbl = "paste cookie (c)";
        let pw = Span::raw(prof_lbl).width() as u16;
        let sw = Span::raw(sep).width() as u16;
        let cw = Span::raw(paste_lbl).width() as u16;
        let total = pw + sw + cw;
        if total <= area.width {
            let x0 = area.x + (area.width - total) / 2;
            let prof_rect = Rect::new(x0, y, pw, 1);
            let paste_rect = Rect::new(x0 + pw + sw, y, cw, 1);
            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(prof_lbl, Style::new().fg(t.graph_text)),
                    Span::styled(sep, Style::new().fg(t.inactive_fg)),
                    Span::styled(paste_lbl, Style::new().fg(t.graph_text)),
                ])),
                Rect::new(x0, y, total, 1),
            );
            app.hits.add(prof_rect, Hit::AuthCustomBrowser);
            app.hits.add(paste_rect, Hit::AuthPasteCookie);
        }
    }
}

// ───────────────────────────── lyrics ─────────────────────────────

pub fn lyrics(f: &mut Frame, app: &mut App, area: Rect) {
    let t = app.theme.clone();
    let Some(track) = app.current().cloned() else {
        centered_msg(f, area, &t, vec![Line::from("nothing playing")]);
        return;
    };
    let [head, gap, body] =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1), Constraint::Min(0)]).areas(area);
    let source = match &app.lyrics {
        LyricsState::Found { source, lyrics, .. } => {
            format!(
                "{source}{}",
                if matches!(lyrics, Lyrics::Synced(_)) {
                    " · synced"
                } else {
                    ""
                }
            )
        }
        _ => String::new(),
    };
    let source_w = if source.is_empty() {
        0
    } else {
        Span::raw(source.as_str()).width() as u16 + 2
    };
    let [title_area, source_area] =
        Layout::horizontal([Constraint::Min(0), Constraint::Length(source_w.min(head.width / 2))]).areas(head);
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                track.title.clone(),
                Style::new().fg(t.title).add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!(" — {}", track.artists), Style::new().fg(t.graph_text)),
        ])),
        title_area,
    );
    f.render_widget(
        Paragraph::new(source)
            .alignment(Alignment::Right)
            .style(Style::new().fg(t.inactive_fg)),
        source_area,
    );
    f.render_widget(
        Paragraph::new("─".repeat(gap.width as usize)).style(Style::new().fg(t.div_line)),
        gap,
    );

    match &app.lyrics {
        LyricsState::None => {}
        LyricsState::Loading { .. } => centered_msg(
            f,
            body,
            &t,
            vec![Line::from(format!("{} looking for lyrics…", spinner()))],
        ),
        LyricsState::Missing => no_lyrics(f, app, body),
        LyricsState::Found {
            lyrics: Lyrics::Plain(text),
            ..
        } => {
            let scroll = app.lyrics_scroll.unwrap_or(0);
            let p = Paragraph::new(text.clone())
                .wrap(Wrap { trim: false })
                .alignment(Alignment::Center)
                .style(Style::new().fg(t.main_fg))
                .scroll((scroll, 0));
            f.render_widget(p, body);
        }
        LyricsState::Found {
            lyrics: Lyrics::Synced(lines),
            ..
        } => {
            // Small lead so the highlight lands as the line is sung.
            let active = active_line(lines, app.player.time + 0.25);
            let w = body.width.max(1) as usize;
            let rows_of = |s: &str| (Span::raw(s).width().max(1)).div_ceil(w) as u16;
            let mut text = Vec::with_capacity(lines.len());
            let mut active_row = 0u16;
            let mut row = 0u16;
            for (i, (_, l)) in lines.iter().enumerate() {
                let content = if l.is_empty() { "♪".to_string() } else { l.clone() };
                let style = match active {
                    Some(a) if a == i => {
                        active_row = row;
                        Style::new().fg(t.hi_fg).add_modifier(Modifier::BOLD)
                    }
                    Some(a) if i < a => Style::new().fg(t.inactive_fg),
                    _ => Style::new().fg(t.main_fg),
                };
                row += rows_of(&content);
                text.push(Line::styled(content, style));
            }
            let auto = active_row.saturating_sub(body.height / 2);
            app.lyrics_auto = auto;
            let scroll = app.lyrics_scroll.unwrap_or(auto);
            f.render_widget(
                Paragraph::new(text)
                    .wrap(Wrap { trim: true })
                    .alignment(Alignment::Center)
                    .scroll((scroll, 0)),
                body,
            );
        }
    }
}

/// The sad pixel cat (the pova mascot) above "no lyrics".
fn no_lyrics(f: &mut Frame, app: &mut App, body: Rect) {
    let t = app.theme.clone();
    let msg = "no lyrics available for this track";
    // Cat image: up to 14 rows, leaving room for the message.
    let img_h = body.height.saturating_sub(3).min(14);
    let mut drew = false;
    if img_h >= 4
        && let Some(cat) = app.cat_art.as_mut()
    {
        let max = Rect { height: img_h, ..body };
        if let Some(size) = cat.size_for(Resize::Scale(None), max.into()) {
            let total_h = size.height + 2;
            let y0 = body.y + body.height.saturating_sub(total_h) / 2;
            let x0 = body.x + body.width.saturating_sub(size.width) / 2;
            let r = Rect::new(x0, y0, size.width.min(body.width), size.height.min(img_h));
            f.render_stateful_widget(StatefulImage::new().resize(Resize::Scale(None)), r, cat);
            f.render_widget(
                Paragraph::new(msg)
                    .alignment(Alignment::Center)
                    .style(Style::new().fg(t.inactive_fg)),
                Rect {
                    y: r.y + r.height + 1,
                    height: 1,
                    ..body
                },
            );
            drew = true;
        }
    }
    if !drew {
        centered_msg(f, body, &t, vec![Line::from(msg)]);
    }
}

#[cfg(test)]
mod tests {
    use super::ellipsize;

    #[test]
    fn ellipsize_counts_columns() {
        assert_eq!(ellipsize("Fine Again", 20), "Fine Again");
        assert_eq!(ellipsize("Fine Again", 6), "Fine …");
        // Wide characters take two columns each.
        assert_eq!(ellipsize("日本語の歌", 5), "日本…");
    }
}
