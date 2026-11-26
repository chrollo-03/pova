//! Modal overlays: help, theme picker, btop-like menu.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Cell, Clear, Paragraph, Row, Table};

use crate::app::{App, Hit, MenuItem, Overlay};
use crate::keymap::Action;
use crate::ui::banner;

fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 2, w, h)
}

fn frame<'a>(t: &crate::theme::Theme, title: &'a str) -> Block<'a> {
    Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(t.hi_fg))
        .style(Style::new().bg(t.main_bg).fg(t.main_fg))
        .title(Line::from(vec![
            Span::styled("─┐", Style::new().fg(t.hi_fg)),
            Span::styled(title, Style::new().fg(t.title).add_modifier(Modifier::BOLD)),
            Span::styled("┌", Style::new().fg(t.hi_fg)),
        ]))
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let t = app.theme.clone();
    let sel = Style::new()
        .bg(t.selected_bg)
        .fg(t.selected_fg)
        .add_modifier(Modifier::BOLD);
    let Some(overlay) = app.overlay.as_mut() else { return };
    let mut extra_hits: Vec<(Rect, Hit)> = Vec::new();
    match overlay {
        Overlay::Help { scroll } => {
            let r = centered(area, 72, area.height.saturating_sub(4).max(10));
            f.render_widget(Clear, r);
            let block = frame(&t, "help").title_bottom(
                Line::from(Span::styled("┐any key closes┌", Style::new().fg(t.inactive_fg)))
                    .alignment(Alignment::Right),
            );
            let inner = block.inner(r);
            f.render_widget(block, r);
            let rows: Vec<Row> = Action::ALL
                .iter()
                .map(|a| {
                    let keys = app.keymap.keys_for(*a).join("  ");
                    Row::new(vec![
                        Cell::from(keys).style(Style::new().fg(t.hi_fg)),
                        Cell::from(a.description()),
                        Cell::from(a.name()).style(Style::new().fg(t.inactive_fg)),
                    ])
                })
                .collect();
            let max = (rows.len() as u16).saturating_sub(inner.height);
            *scroll = (*scroll).min(max);
            let rows: Vec<Row> = rows.into_iter().skip(*scroll as usize).collect();
            let table = Table::new(
                rows,
                [Constraint::Length(20), Constraint::Fill(1), Constraint::Length(18)],
            )
            .column_spacing(2);
            f.render_widget(
                table,
                Rect {
                    x: inner.x + 1,
                    width: inner.width.saturating_sub(2),
                    ..inner
                },
            );
        }
        Overlay::Themes { names, state, .. } => {
            let h = (names.len() as u16 + 2).min(area.height.saturating_sub(2)).max(5);
            let r = centered(area, 40, h);
            f.render_widget(Clear, r);
            let block = frame(&t, "themes").title_bottom(
                Line::from(Span::styled(
                    "┐enter keep · esc revert┌",
                    Style::new().fg(t.inactive_fg),
                ))
                .alignment(Alignment::Right),
            );
            let inner = block.inner(r);
            f.render_widget(block, r);
            let rows: Vec<Row> = names
                .iter()
                .map(|(n, p)| {
                    let origin = if p.is_none() { "built-in" } else { "" };
                    Row::new(vec![
                        Cell::from(n.as_str()),
                        Cell::from(origin).style(Style::new().fg(t.inactive_fg)),
                    ])
                })
                .collect();
            let table = Table::new(rows, [Constraint::Fill(1), Constraint::Length(9)]).row_highlight_style(sel);
            f.render_stateful_widget(table, inner, state);
            let off = state.offset();
            for row_i in 0..(inner.height as usize).min(names.len().saturating_sub(off)) {
                extra_hits.push((
                    Rect::new(inner.x, inner.y + row_i as u16, inner.width, 1),
                    Hit::ThemeItem(off + row_i),
                ));
            }
        }
        Overlay::Menu { state } => {
            let state = *state;
            draw_menu(f, app, area, state.selected().unwrap_or(0));
        }
        Overlay::Confirm { question, on_yes, yes } => {
            let width = 60.min(area.width.saturating_sub(4)).max(24);
            let lines = crate::ui::wrap_text(question, width.saturating_sub(4) as usize);
            let text_h = lines.len() as u16;
            // Border, padding, question, blank line, buttons, padding, border.
            let h = text_h + 6;
            let r = centered(area, width, h);
            f.render_widget(Clear, r);
            let block = frame(&t, "are you sure?").title_bottom(
                Line::from(Span::styled("┐y yes · n / esc no┌", Style::new().fg(t.inactive_fg)))
                    .alignment(Alignment::Right),
            );
            let inner = block.inner(r);
            f.render_widget(block, r);
            let text: Vec<Line> = lines.into_iter().map(Line::from).collect();
            f.render_widget(
                Paragraph::new(text).alignment(Alignment::Center),
                Rect {
                    x: inner.x + 1,
                    y: inner.y + 1,
                    width: inner.width.saturating_sub(2),
                    height: text_h.min(inner.height),
                },
            );
            let button = |label: &'static str, on: bool, danger: bool| {
                let style = if on && danger {
                    Style::new()
                        .bg(t.error)
                        .fg(crate::theme::text_on(t.error))
                        .add_modifier(Modifier::BOLD)
                } else if on {
                    sel
                } else {
                    Style::new().fg(t.inactive_fg)
                };
                Span::styled(label, style)
            };
            let yes_lbl = on_yes.confirm_label();
            let no_lbl = "  No  ";
            let buttons = Line::from(vec![
                button(yes_lbl, *yes, true),
                Span::raw("    "),
                button(no_lbl, !*yes, false),
            ]);
            let btn_y = (inner.y + text_h + 2).min(inner.y + inner.height.saturating_sub(1));
            f.render_widget(
                Paragraph::new(buttons).alignment(Alignment::Center),
                Rect {
                    y: btn_y,
                    height: 1,
                    ..inner
                },
            );
            let yes_w = Span::raw(yes_lbl).width() as u16;
            let no_w = Span::raw(no_lbl).width() as u16;
            let total_w = yes_w + 4 + no_w;
            if total_w <= inner.width {
                let bx = inner.x + (inner.width - total_w) / 2;
                extra_hits.push((Rect::new(bx, btn_y, yes_w, 1), Hit::Confirm(true)));
                extra_hits.push((Rect::new(bx + yes_w + 4, btn_y, no_w, 1), Hit::Confirm(false)));
            }
        }
        Overlay::AddToPlaylist {
            track,
            playlists,
            state,
            naming,
        } => {
            let rows_n = playlists.as_ref().map_or(2, |p| p.len() + 1) as u16;
            let h = (rows_n + 4).min(area.height.saturating_sub(2)).max(7);
            let r = centered(area, 56, h);
            f.render_widget(Clear, r);
            let hint = if naming.is_some() {
                "┐enter create · esc back┌"
            } else {
                "┐enter add · esc close┌"
            };
            let block = frame(&t, "add to playlist").title_bottom(
                Line::from(Span::styled(hint, Style::new().fg(t.inactive_fg))).alignment(Alignment::Right),
            );
            let inner = block.inner(r);
            f.render_widget(block, r);
            let inner = Rect {
                x: inner.x + 1,
                width: inner.width.saturating_sub(2),
                ..inner
            };
            // Which song, on top.
            let song = format!("“{}” · {}", track.title, track.artists);
            f.render_widget(
                Paragraph::new(song).style(Style::new().fg(t.title).add_modifier(Modifier::BOLD)),
                Rect { height: 1, ..inner },
            );
            let body = Rect {
                y: inner.y + 2,
                height: inner.height.saturating_sub(2),
                ..inner
            };
            if let Some(name) = naming {
                let lines = vec![
                    Line::from(Span::styled(
                        "Name of the new private playlist:",
                        Style::new().fg(t.inactive_fg),
                    )),
                    Line::from(vec![
                        Span::styled(name.clone(), Style::new().fg(t.main_fg)),
                        Span::styled("█", Style::new().fg(t.hi_fg)),
                    ]),
                ];
                f.render_widget(Paragraph::new(lines), body);
                return;
            }
            let mut rows = vec![Row::new(vec![
                Cell::from("+ New playlist…").style(Style::new().fg(t.hi_fg)),
            ])];
            match playlists {
                None => rows.push(Row::new(vec![
                    Cell::from("loading your playlists…").style(Style::new().fg(t.inactive_fg)),
                ])),
                Some(list) => rows.extend(list.iter().map(|p| {
                    Row::new(vec![
                        Cell::from(p.title.as_str()),
                        Cell::from(p.subtitle.as_str()).style(Style::new().fg(t.inactive_fg)),
                    ])
                })),
            }
            let row_count = rows.len();
            let table = Table::new(rows, [Constraint::Fill(1), Constraint::Length(20)]).row_highlight_style(sel);
            f.render_stateful_widget(table, body, state);
            let off = state.offset();
            for row_i in 0..(body.height as usize).min(row_count.saturating_sub(off)) {
                extra_hits.push((
                    Rect::new(body.x, body.y + row_i as u16, body.width, 1),
                    Hit::PlaylistPick(off + row_i),
                ));
            }
        }
        Overlay::Accounts { items, state } => {
            let rows_n = items.as_ref().map_or(1, |v| v.len().max(1)) as u16;
            let h = (rows_n + 4).min(area.height.saturating_sub(2)).max(6);
            let r = centered(area, 60, h);
            f.render_widget(Clear, r);
            let block = frame(&t, "switch YouTube account").title_bottom(
                Line::from(Span::styled(
                    "┐enter switch · esc close┌",
                    Style::new().fg(t.inactive_fg),
                ))
                .alignment(Alignment::Right),
            );
            let inner = block.inner(r);
            f.render_widget(block, r);
            let inner = Rect {
                x: inner.x + 1,
                width: inner.width.saturating_sub(2),
                ..inner
            };
            let list_len = items.as_ref().map_or(0, Vec::len);
            let rows: Vec<Row> = match items {
                None => vec![Row::new(vec![
                    Cell::from("loading your YouTube accounts & channels…").style(Style::new().fg(t.inactive_fg)),
                ])],
                Some(list) if list.is_empty() => vec![Row::new(vec![
                    Cell::from("no accounts found").style(Style::new().fg(t.inactive_fg)),
                ])],
                Some(list) => list
                    .iter()
                    .map(|a| {
                        let mark = if a.active { "● " } else { "  " };
                        Row::new(vec![
                            Cell::from(format!("{mark}{}", a.name)),
                            Cell::from(a.subtitle.as_str()).style(Style::new().fg(t.inactive_fg)),
                        ])
                    })
                    .collect(),
            };
            let table = Table::new(rows, [Constraint::Fill(1), Constraint::Length(24)]).row_highlight_style(sel);
            f.render_stateful_widget(table, inner, state);
            let off = state.offset();
            for row_i in 0..(inner.height as usize).min(list_len.saturating_sub(off)) {
                extra_hits.push((
                    Rect::new(inner.x, inner.y + row_i as u16, inner.width, 1),
                    Hit::AccountItem(off + row_i),
                ));
            }
        }
        Overlay::SleepTimer { state, custom } => {
            let presets = crate::sleep::SleepPreset::MENU_ITEMS;
            let h = (presets.len() as u16 + 5).min(area.height.saturating_sub(2)).max(9);
            let r = centered(area, 52, h);
            f.render_widget(Clear, r);
            let hint = if custom.is_some() {
                "┐enter set · esc back┌"
            } else {
                "┐+/- 5m · enter select · esc close┌"
            };
            let block = frame(&t, "sleep timer").title_bottom(
                Line::from(Span::styled(hint, Style::new().fg(t.inactive_fg))).alignment(Alignment::Right),
            );
            let inner = block.inner(r);
            f.render_widget(block, r);
            let inner = Rect {
                x: inner.x + 1,
                width: inner.width.saturating_sub(2),
                ..inner
            };
            let now = std::time::Instant::now();
            let status_line = match &app.sleep.mode {
                crate::sleep::SleepMode::Off => Line::from(Span::styled(
                    "Timer is off",
                    Style::new().fg(t.inactive_fg),
                )),
                crate::sleep::SleepMode::EndOfTrack => Line::from(vec![
                    Span::styled("☾ ", Style::new().fg(t.hi_fg).add_modifier(Modifier::BOLD)),
                    Span::styled(
                        "Stopping at end of current track",
                        Style::new().fg(t.accent).add_modifier(Modifier::BOLD),
                    ),
                ]),
                crate::sleep::SleepMode::Countdown { .. } => {
                    let rem = app.sleep.badge(now).unwrap_or_default();
                    let fade = if app.sleep.fade_secs > 0 {
                        format!(" ({}s fade-out)", app.sleep.fade_secs)
                    } else {
                        String::new()
                    };
                    Line::from(vec![
                        Span::styled("☾ ", Style::new().fg(t.hi_fg).add_modifier(Modifier::BOLD)),
                        Span::styled(
                            format!("Pausing in {rem}"),
                            Style::new().fg(t.accent).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(fade, Style::new().fg(t.inactive_fg)),
                    ])
                }
            };
            f.render_widget(Paragraph::new(status_line), Rect { height: 1, ..inner });
            if inner.height >= 2 {
                f.render_widget(
                    Paragraph::new("─".repeat(inner.width as usize)).style(Style::new().fg(t.div_line)),
                    Rect {
                        y: inner.y + 1,
                        height: 1,
                        ..inner
                    },
                );
            }
            let body = Rect {
                y: inner.y + 2,
                height: inner.height.saturating_sub(2),
                ..inner
            };
            if let Some(text) = custom {
                let lines = vec![
                    Line::from(Span::styled(
                        "Pause playback after how many minutes? (1–720):",
                        Style::new().fg(t.inactive_fg),
                    )),
                    Line::from(""),
                    Line::from(vec![
                        Span::styled(text.clone(), Style::new().fg(t.title).add_modifier(Modifier::BOLD)),
                        Span::styled("█", Style::new().fg(t.hi_fg)),
                        Span::styled(" min", Style::new().fg(t.inactive_fg)),
                    ]),
                ];
                f.render_widget(Paragraph::new(lines), body);
                return;
            }
            let rows: Vec<Row> = presets
                .iter()
                .map(|p| {
                    let tag = match p {
                        crate::sleep::SleepPreset::EndOfTrack if app.sleep.is_end_of_track() => "active",
                        crate::sleep::SleepPreset::Off if !app.sleep.is_active() => "active",
                        _ => "",
                    };
                    Row::new(vec![
                        Cell::from(p.label()),
                        Cell::from(tag).style(Style::new().fg(t.accent)),
                    ])
                })
                .collect();
            let table = Table::new(rows, [Constraint::Fill(1), Constraint::Length(8)]).row_highlight_style(sel);
            f.render_stateful_widget(table, body, state);
            let off = state.offset();
            for row_i in 0..(body.height as usize).min(presets.len().saturating_sub(off)) {
                extra_hits.push((
                    Rect::new(body.x, body.y + row_i as u16, body.width, 1),
                    Hit::SleepItem(off + row_i),
                ));
            }
        }
        Overlay::Equalizer { band } => {
            let b = *band;
            draw_equalizer(f, app, area, b);
            return;
        }
        Overlay::Interlink {
            track,
            is_current,
            actions,
            state,
        } => {
            let mut signal_rows = if *is_current {
                app.player.audio.signal_path_rows(
                    app.cfg.audio_quality,
                    app.cfg.normalize_loudness,
                    &app.cfg.eq,
                    app.cfg.playback_speed,
                    app.cfg.pitch_correction,
                )
            } else {
                Vec::new()
            };
            if *is_current && app.offline.contains(&track.video_id) {
                signal_rows.insert(0, ("Source".into(), "Local file (offline · 0 ms)".into()));
            }
            let sig_h = if signal_rows.is_empty() {
                0
            } else {
                signal_rows.len() as u16 + 2
            };
            let h = (actions.len() as u16 + sig_h + 6)
                .min(area.height.saturating_sub(2))
                .max(10);
            let w = 68.min(area.width.saturating_sub(2)).max(24);
            let r = centered(area, w, h);
            f.render_widget(Clear, r);
            let title = if *is_current {
                "interlink & signal path"
            } else {
                "interlink"
            };
            let block = frame(&t, title).title_bottom(
                Line::from(Span::styled(
                    "┐↑↓ select · enter open/copy · esc close┌",
                    Style::new().fg(t.inactive_fg),
                ))
                .alignment(Alignment::Right),
            );
            let inner = block.inner(r);
            f.render_widget(block, r);
            let inner = Rect {
                x: inner.x + 1,
                width: inner.width.saturating_sub(2),
                ..inner
            };
            let mut y = inner.y;
            let bottom = inner.y + inner.height;
            if y < bottom {
                let header = format!("“{}” · {}", track.title, track.artists);
                f.render_widget(
                    Paragraph::new(header).style(Style::new().fg(t.title).add_modifier(Modifier::BOLD)),
                    Rect::new(inner.x, y, inner.width, 1),
                );
                y += 1;
            }
            if !signal_rows.is_empty() && y + 2 < bottom {
                f.render_widget(
                    Paragraph::new("─".repeat(inner.width as usize)).style(Style::new().fg(t.div_line)),
                    Rect::new(inner.x, y, inner.width, 1),
                );
                y += 1;
                for (k, v) in signal_rows {
                    if y + 3 >= bottom {
                        break;
                    }
                    let line = Line::from(vec![
                        Span::styled(format!("{k:<14}"), Style::new().fg(t.inactive_fg)),
                        Span::styled(v, Style::new().fg(t.accent)),
                    ]);
                    f.render_widget(Paragraph::new(line), Rect::new(inner.x, y, inner.width, 1));
                    y += 1;
                }
            }
            if y < bottom {
                f.render_widget(
                    Paragraph::new("─".repeat(inner.width as usize)).style(Style::new().fg(t.div_line)),
                    Rect::new(inner.x, y, inner.width, 1),
                );
                y += 1;
            }
            let body = Rect {
                y,
                height: bottom.saturating_sub(y),
                ..inner
            };
            let rows: Vec<Row> = actions
                .iter()
                .map(|a| {
                    let (lbl, sub) = a.label();
                    Row::new(vec![
                        Cell::from(lbl),
                        Cell::from(sub).style(Style::new().fg(t.graph_text)),
                    ])
                })
                .collect();
            let table = Table::new(rows, [Constraint::Fill(1), Constraint::Length(28)]).row_highlight_style(sel);
            f.render_stateful_widget(table, body, state);
            let off = state.offset();
            for row_i in 0..(body.height as usize).min(actions.len().saturating_sub(off)) {
                extra_hits.push((
                    Rect::new(body.x, body.y + row_i as u16, body.width, 1),
                    Hit::InterlinkItem(off + row_i),
                ));
            }
        }
        Overlay::Stats { tab, state } => {
            let cur_tab = *tab;
            let mut st = *state;
            draw_stats(f, app, area, cur_tab, &mut st);
            if let Some(Overlay::Stats { state, .. }) = app.overlay.as_mut() {
                *state = st;
            }
            return;
        }
    }
    for (r, h) in extra_hits {
        app.hits.add(r, h);
    }
}

/// Spotify Wrapped Mini — local listening stats with btop-style horizontal bars.
fn draw_stats(
    f: &mut Frame,
    app: &mut App,
    area: Rect,
    tab: crate::app::StatsTab,
    state: &mut ratatui::widgets::TableState,
) {
    let t = app.theme.clone();
    let sel = Style::new()
        .bg(t.selected_bg)
        .fg(t.selected_fg)
        .add_modifier(Modifier::BOLD);
    let w = 78.min(area.width.saturating_sub(2)).max(28);
    let h = 24.min(area.height.saturating_sub(2)).max(11);
    let r = centered(area, w, h);
    f.render_widget(Clear, r);
    let block = frame(&t, "listening stats · wrapped mini").title_bottom(
        Line::from(Span::styled(
            "┐←→ tab · ↑↓ select · enter play/open · esc close┌",
            Style::new().fg(t.inactive_fg),
        ))
        .alignment(Alignment::Right),
    );
    let inner = block.inner(r);
    f.render_widget(block, r);
    let inner = Rect {
        x: inner.x + 1,
        width: inner.width.saturating_sub(2),
        ..inner
    };
    if inner.height < 5 || inner.width < 12 {
        return;
    }

    let now = crate::stats::now_unix();
    let streak = app.stats.streak_days(now);
    let summary = Line::from(vec![
        Span::styled(
            format!("{} plays", app.stats.total_plays),
            Style::new().fg(t.title).add_modifier(Modifier::BOLD),
        ),
        Span::styled("  ·  ", Style::new().fg(t.inactive_fg)),
        Span::styled(
            crate::stats::format_hours(app.stats.total_seconds),
            Style::new().fg(t.accent).add_modifier(Modifier::BOLD),
        ),
        Span::styled("  ·  ", Style::new().fg(t.inactive_fg)),
        Span::styled(
            format!("{streak}d streak"),
            Style::new().fg(t.hi_fg).add_modifier(Modifier::BOLD),
        ),
        Span::styled("  ·  ", Style::new().fg(t.inactive_fg)),
        Span::styled(
            format!("{} artists", app.stats.artists.len()),
            Style::new().fg(t.graph_text),
        ),
    ]);
    f.render_widget(Paragraph::new(summary), Rect::new(inner.x, inner.y, inner.width, 1));

    // Sub-tabs row (clickable)
    let tab_y = inner.y + 1;
    let mut tx = inner.x;
    let mut tab_spans = Vec::new();
    for st in crate::app::StatsTab::ALL {
        let text = format!(" {} ", st.label());
        let tw = Span::raw(text.as_str()).width() as u16;
        let style = if st == tab {
            sel
        } else {
            Style::new().fg(t.graph_text)
        };
        if tx + tw <= inner.x + inner.width {
            app.hits.add(Rect::new(tx, tab_y, tw, 1), Hit::StatsTab(st));
        }
        tx += tw + 1;
        tab_spans.push(Span::styled(text, style));
        tab_spans.push(Span::raw(" "));
    }
    f.render_widget(
        Paragraph::new(Line::from(tab_spans)),
        Rect::new(inner.x, tab_y, inner.width, 1),
    );
    f.render_widget(
        Paragraph::new("─".repeat(inner.width as usize)).style(Style::new().fg(t.div_line)),
        Rect::new(inner.x, inner.y + 2, inner.width, 1),
    );

    let body = Rect {
        y: inner.y + 3,
        height: inner.height.saturating_sub(3),
        ..inner
    };
    let bar = |ratio: f64, width: usize| -> String {
        let filled = ((ratio.clamp(0.0, 1.0)) * width as f64).round() as usize;
        format!("{}{}", "█".repeat(filled), "░".repeat(width.saturating_sub(filled)))
    };

    let (rows, row_count): (Vec<Row>, usize) = match tab {
        crate::app::StatsTab::TopTracks => {
            let list = app.stats.top_tracks(20);
            let max_plays = list.first().map_or(1, |x| x.plays).max(1) as f64;
            let n = list.len();
            let rows = list
                .into_iter()
                .enumerate()
                .map(|(i, s)| {
                    let ratio = s.plays as f64 / max_plays;
                    Row::new(vec![
                        Cell::from(format!("{:>2}", i + 1)).style(Style::new().fg(t.graph_text)),
                        Cell::from(s.track.title.clone()),
                        Cell::from(s.track.artists.clone()).style(Style::new().fg(t.graph_text)),
                        Cell::from(bar(ratio, 10)).style(Style::new().fg(t.viz_color(ratio as f32))),
                        Cell::from(format!("{}x · {}", s.plays, crate::stats::format_hours(s.seconds)))
                            .style(Style::new().fg(t.inactive_fg)),
                    ])
                })
                .collect();
            (rows, n)
        }
        crate::app::StatsTab::TopArtists => {
            let list = app.stats.top_artists(20);
            let max_plays = list.first().map_or(1, |x| x.plays).max(1) as f64;
            let n = list.len();
            let rows = list
                .into_iter()
                .enumerate()
                .map(|(i, a)| {
                    let ratio = a.plays as f64 / max_plays;
                    Row::new(vec![
                        Cell::from(format!("{:>2}", i + 1)).style(Style::new().fg(t.graph_text)),
                        Cell::from(a.name.clone()),
                        Cell::from("").style(Style::new().fg(t.graph_text)),
                        Cell::from(bar(ratio, 10)).style(Style::new().fg(t.viz_color(ratio as f32))),
                        Cell::from(format!("{}x · {}", a.plays, crate::stats::format_hours(a.seconds)))
                            .style(Style::new().fg(t.inactive_fg)),
                    ])
                })
                .collect();
            (rows, n)
        }
        crate::app::StatsTab::Recent => {
            let list = app.stats.recent_tracks(25);
            let n = list.len();
            let rows = list
                .into_iter()
                .enumerate()
                .map(|(i, s)| {
                    Row::new(vec![
                        Cell::from(format!("{:>2}", i + 1)).style(Style::new().fg(t.graph_text)),
                        Cell::from(s.track.title.clone()),
                        Cell::from(s.track.artists.clone()).style(Style::new().fg(t.graph_text)),
                        Cell::from(format!("{} plays", s.plays)).style(Style::new().fg(t.accent)),
                        Cell::from(crate::stats::format_hours(s.seconds)).style(Style::new().fg(t.inactive_fg)),
                    ])
                })
                .collect();
            (rows, n)
        }
    };

    if row_count == 0 {
        f.render_widget(
            Paragraph::new("Play songs for 25+ seconds to build your Wrapped Mini stats.")
                .alignment(Alignment::Center)
                .style(Style::new().fg(t.inactive_fg)),
            Rect {
                y: body.y + body.height / 2,
                height: 1,
                ..body
            },
        );
        return;
    }

    let table = Table::new(
        rows,
        [
            Constraint::Length(2),
            Constraint::Fill(4),
            Constraint::Fill(3),
            Constraint::Length(10),
            Constraint::Length(13),
        ],
    )
    .column_spacing(1)
    .row_highlight_style(sel);
    f.render_stateful_widget(table, body, state);
    let off = state.offset();
    for row_i in 0..(body.height as usize).min(row_count.saturating_sub(off)) {
        app.hits.add(
            Rect::new(body.x, body.y + row_i as u16, body.width, 1),
            Hit::StatsItem(off + row_i),
        );
    }
}

/// Spotify/Tidal-style 10-band interactive graphic equalizer overlay.
fn draw_equalizer(f: &mut Frame, app: &mut App, area: Rect, selected_band: usize) {
    let t = app.theme.clone();
    let w = 68.min(area.width.saturating_sub(2)).max(36);
    let h = 20.min(area.height.saturating_sub(2)).max(11);
    let r = centered(area, w, h);
    f.render_widget(Clear, r);
    let block = frame(&t, "10-band equalizer").title_bottom(
        Line::from(Span::styled(
            "┐←→ band · ↑↓ ±1dB · p preset · space toggle · 0 flat · esc close┌",
            Style::new().fg(t.inactive_fg),
        ))
        .alignment(Alignment::Right),
    );
    let inner = block.inner(r);
    f.render_widget(block, r);
    let inner = Rect {
        x: inner.x + 1,
        width: inner.width.saturating_sub(2),
        ..inner
    };
    if inner.height < 6 || inner.width < 20 {
        return;
    }

    // Header controls row: [● ON/OFF]   ‹ Preset ›   [0 flat]
    let on_lbl = if app.cfg.eq.enabled { " ● ON  " } else { " ○ OFF " };
    let on_style = if app.cfg.eq.enabled {
        Style::new().bg(t.accent).fg(crate::theme::text_on(t.accent)).add_modifier(Modifier::BOLD)
    } else {
        Style::new().fg(t.inactive_fg)
    };
    let prev_lbl = " ‹ ";
    let preset_lbl = format!(" {} ", app.cfg.eq.preset.label());
    let next_lbl = " › ";
    let reset_lbl = " [0 flat] ";
    let mut hx = inner.x;
    let hy = inner.y;
    let on_w = Span::raw(on_lbl).width() as u16;
    let prev_w = Span::raw(prev_lbl).width() as u16;
    let preset_w = Span::raw(preset_lbl.as_str()).width() as u16;
    let next_w = Span::raw(next_lbl).width() as u16;
    let reset_w = Span::raw(reset_lbl).width() as u16;

    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(on_lbl, on_style),
            Span::raw("  "),
            Span::styled(prev_lbl, Style::new().fg(t.hi_fg).add_modifier(Modifier::BOLD)),
            Span::styled(preset_lbl, Style::new().fg(t.title).add_modifier(Modifier::BOLD)),
            Span::styled(next_lbl, Style::new().fg(t.hi_fg).add_modifier(Modifier::BOLD)),
            Span::raw("  "),
            Span::styled(reset_lbl, Style::new().fg(t.graph_text)),
        ])),
        Rect::new(hx, hy, inner.width, 1),
    );
    if hx + on_w <= inner.x + inner.width {
        app.hits.add(Rect::new(hx, hy, on_w, 1), Hit::EqToggle);
    }
    hx += on_w + 2;
    if hx + prev_w <= inner.x + inner.width {
        app.hits.add(Rect::new(hx, hy, prev_w, 1), Hit::EqPrevPreset);
    }
    hx += prev_w + preset_w;
    if hx + next_w <= inner.x + inner.width {
        app.hits.add(Rect::new(hx, hy, next_w, 1), Hit::EqNextPreset);
    }
    hx += next_w + 2;
    if hx + reset_w <= inner.x + inner.width {
        app.hits.add(Rect::new(hx, hy, reset_w, 1), Hit::EqReset);
    }

    // Divider
    f.render_widget(
        Paragraph::new("─".repeat(inner.width as usize)).style(Style::new().fg(t.div_line)),
        Rect::new(inner.x, inner.y + 1, inner.width, 1),
    );

    // 10-band vertical bar meters
    let meter_top = inner.y + 3;
    let meter_h = inner.height.saturating_sub(5).max(3);
    let label_y = meter_top + meter_h;
    let axis_w = 5u16;
    let avail_w = inner.width.saturating_sub(axis_w);
    let col_w = (avail_w / 10).max(2);
    let grid_x = inner.x + axis_w + avail_w.saturating_sub(col_w * 10) / 2;

    // dB axis labels (+12, 0, -12)
    f.render_widget(
        Paragraph::new("+12").style(Style::new().fg(t.inactive_fg)),
        Rect::new(inner.x, meter_top, axis_w, 1),
    );
    f.render_widget(
        Paragraph::new(" 0dB").style(Style::new().fg(t.graph_text)),
        Rect::new(inner.x, meter_top + meter_h / 2, axis_w, 1),
    );
    f.render_widget(
        Paragraph::new("-12").style(Style::new().fg(t.inactive_fg)),
        Rect::new(inner.x, meter_top + meter_h.saturating_sub(1), axis_w, 1),
    );

    for i in 0..10 {
        let gain = app.cfg.eq.gains[i];
        let is_sel = i == selected_band;
        let cx = grid_x + (i as u16) * col_w;
        // Exact gain value above each bar
        let g_str = if gain > 0.0 {
            format!("+{gain:.0}")
        } else {
            format!("{gain:.0}")
        };
        let g_style = if is_sel {
            Style::new().fg(t.hi_fg).add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(t.inactive_fg)
        };
        f.render_widget(
            Paragraph::new(g_str).alignment(Alignment::Center).style(g_style),
            Rect::new(cx, inner.y + 2, col_w, 1),
        );

        // Bar cells from top (+12dB) to bottom (-12dB)
        let norm = ((gain - crate::audio::EQ_MIN_DB) / (crate::audio::EQ_MAX_DB - crate::audio::EQ_MIN_DB))
            .clamp(0.0, 1.0);
        let filled = (norm * meter_h as f32).round() as u16;
        let zero_row = meter_h / 2;
        for row_i in 0..meter_h {
            let from_bottom = meter_h - 1 - row_i;
            let lit = from_bottom < filled;
            let bar_cell = if lit {
                let color = if !app.cfg.eq.enabled {
                    t.inactive_fg
                } else {
                    t.viz_color(from_bottom as f32 / meter_h.max(1) as f32)
                };
                Span::styled(
                    if is_sel { "██" } else { "▓▓" },
                    Style::new().fg(color),
                )
            } else if row_i == zero_row {
                Span::styled("──", Style::new().fg(t.div_line))
            } else {
                Span::styled("··", Style::new().fg(t.meter_bg))
            };
            f.render_widget(
                Paragraph::new(bar_cell).alignment(Alignment::Center),
                Rect::new(cx, meter_top + row_i, col_w, 1),
            );
        }

        // Frequency label at bottom
        let freq_style = if is_sel {
            Style::new()
                .bg(t.selected_bg)
                .fg(t.selected_fg)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(t.graph_text)
        };
        if label_y < inner.y + inner.height {
            f.render_widget(
                Paragraph::new(crate::audio::EQ_LABELS[i])
                    .alignment(Alignment::Center)
                    .style(freq_style),
                Rect::new(cx, label_y, col_w, 1),
            );
        }
        app.hits.add(
            Rect::new(cx, inner.y + 2, col_w, meter_h + 2),
            Hit::EqBand(i),
        );
    }
}

/// btop-style Esc menu: the pova logo banner above the options.
fn draw_menu(f: &mut Frame, app: &mut App, area: Rect, selected: usize) {
    let t = app.theme.clone();
    let items: Vec<String> = MenuItem::ALL.iter().map(|i| app.menu_label(*i)).collect();
    let items_w = items
        .iter()
        .map(|s| Span::raw(s.as_str()).width() as u16)
        .max()
        .unwrap_or(10)
        + 6;
    let items_h = items.len() as u16;
    // Largest banner that fits, or none on tiny terminals.
    let scale = [2usize, 1]
        .into_iter()
        .find(|&s| banner::width(s) + 4 <= area.width && (s as u16 * 4 + 2) + items_h + 6 <= area.height);
    let logo = scale.map(|s| banner::lines(s, &t)).unwrap_or_default();
    let logo_w = scale.map(banner::width).unwrap_or(0);
    let panel_w = logo_w.max(items_w) + 4;
    let panel_h = (logo.len() as u16 + items_h + 5).min(area.height);
    let r = centered(area, panel_w, panel_h);
    f.render_widget(Clear, r);
    f.render_widget(Block::new().style(Style::new().bg(t.main_bg)), r);

    let mut y = r.y + 1;
    let bottom = r.y + r.height;
    if !logo.is_empty() {
        let x = r.x + (r.width.saturating_sub(logo_w)) / 2;
        for line in logo {
            if y >= bottom {
                break;
            }
            f.render_widget(Paragraph::new(line), Rect::new(x, y, logo_w.min(r.width), 1));
            y += 1;
        }
        y += 1;
    }
    let avail_rows = bottom.saturating_sub(y).saturating_sub(2).max(1) as usize;
    let start = if items.len() > avail_rows {
        selected
            .saturating_sub(avail_rows / 2)
            .min(items.len() - avail_rows)
    } else {
        0
    };
    let end = (start + avail_rows).min(items.len());
    let accent = t.logo_color(0.5);
    for (i, label) in items.iter().enumerate().skip(start).take(end - start) {
        if y >= bottom {
            break;
        }
        let row = Rect::new(r.x + (r.width.saturating_sub(items_w)) / 2, y, items_w.min(r.width), 1);
        let style = if i == selected {
            Style::new()
                .bg(accent)
                .fg(crate::theme::text_on(accent))
                .add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(t.main_fg)
        };
        // One left-aligned column so the ‹ value › parts line up.
        f.render_widget(Paragraph::new(format!("   {label}")).style(style), row);
        app.hits.add(row, Hit::MenuItem(i));
        y += 1;
    }
    let hint = "↑↓ select · enter/←→ change · esc close";
    if y + 1 < bottom {
        f.render_widget(
            Paragraph::new(hint)
                .alignment(Alignment::Center)
                .style(Style::new().fg(t.inactive_fg)),
            Rect::new(r.x, y + 1, r.width, 1),
        );
    }
}
