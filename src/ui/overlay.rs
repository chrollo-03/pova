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
    }
    for (r, h) in extra_hits {
        app.hits.add(r, h);
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
    let panel_h = logo.len() as u16 + items_h + 5;
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
    }
    y += 1;
    let accent = t.logo_color(0.5);
    for (i, label) in items.iter().enumerate() {
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
