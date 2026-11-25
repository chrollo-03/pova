//! Rendering: btop-style boxes and layout.

mod banner;
mod kitty_fix;
mod overlay;
mod tabs;

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};

use crate::app::{App, Hit, HitMap, Tab};
use crate::keymap::Action;
use crate::model::format_time;
use crate::theme::Theme;
use crate::viz::render::{VizWidget, bars_needed};

const CONTROLS_HEIGHT: u16 = 5;

pub struct Areas {
    pub viz: Option<Rect>,
    pub player: Option<Rect>,
    pub controls: Option<Rect>,
}

/// Decide where each box goes. Wide terminals: visualizer left, player +
/// controls stacked on the right (like the sketch). Narrow: everything stacked.
pub fn layout(area: Rect, viz: bool, player: bool, controls: bool) -> Areas {
    let ctl = |on: bool| if on { CONTROLS_HEIGHT } else { 0 };
    let mut a = Areas {
        viz: None,
        player: None,
        controls: None,
    };
    if player {
        let (main, side) = if viz && area.width >= 90 {
            let right = (area.width * 38 / 100).max(44).min(area.width - 20);
            let [l, r] = Layout::horizontal([Constraint::Min(20), Constraint::Length(right)]).areas(area);
            (Some(l), r)
        } else if viz {
            let [top, rest] = Layout::vertical([Constraint::Percentage(35), Constraint::Min(8)]).areas(area);
            (Some(top), rest)
        } else {
            (None, area)
        };
        a.viz = main;
        let [p, c] = Layout::vertical([Constraint::Min(6), Constraint::Length(ctl(controls))]).areas(side);
        a.player = Some(p);
        a.controls = controls.then_some(c);
    } else {
        let [v, c] = Layout::vertical([Constraint::Min(0), Constraint::Length(ctl(controls))]).areas(area);
        a.viz = viz.then_some(v);
        a.controls = controls.then_some(c);
    }
    a
}

/// Superscript digits used for btop-style box hotkeys.
fn superscript(n: u8) -> &'static str {
    ["⁰", "¹", "²", "³", "⁴", "⁵", "⁶", "⁷", "⁸", "⁹"][n as usize % 10]
}

/// A label embedded in a border: `┐¹title┌`.
fn border_label<'a>(theme: &Theme, border: Color, hotkey: Option<u8>, text: &'a str, active: bool) -> Vec<Span<'a>> {
    let mut v = vec![Span::styled("┐", Style::new().fg(border))];
    if let Some(n) = hotkey {
        v.push(Span::styled(superscript(n), Style::new().fg(theme.hi_fg)));
    }
    let style = if active {
        Style::new().fg(theme.title).add_modifier(Modifier::BOLD)
    } else {
        Style::new().fg(theme.graph_text)
    };
    v.push(Span::styled(text, style));
    v.push(Span::styled("┌", Style::new().fg(border)));
    v
}

/// A tab label with its hotkey letter highlighted, btop style.
fn tab_label<'a>(theme: &Theme, border: Color, label: &'a str, key_at: usize, active: bool) -> Vec<Span<'a>> {
    let base = if active {
        Style::new().fg(theme.title).add_modifier(Modifier::BOLD)
    } else {
        Style::new().fg(theme.graph_text)
    };
    let (pre, rest) = label.split_at(key_at);
    let mut chars = rest.chars();
    let key = chars.next().map(String::from).unwrap_or_default();
    let post: String = chars.collect();
    vec![
        Span::styled("┐", Style::new().fg(border)),
        Span::styled(pre, base),
        Span::styled(key, base.fg(theme.hi_fg).add_modifier(Modifier::UNDERLINED)),
        Span::styled(post, base),
        Span::styled("┌", Style::new().fg(border)),
    ]
}

fn boxed<'a>(app: &App, color: Color) -> Block<'a> {
    Block::new()
        .borders(Borders::ALL)
        .border_type(if app.cfg.rounded_corners {
            BorderType::Rounded
        } else {
            BorderType::Plain
        })
        .border_style(Style::new().fg(color))
        .style(Style::new().bg(app.theme.main_bg).fg(app.theme.main_fg))
}

fn spans_width(spans: &[Span]) -> u16 {
    spans.iter().map(|s| s.width() as u16).sum()
}

pub fn draw(f: &mut Frame, app: &mut App) -> usize {
    app.hits = HitMap::default();
    let area = f.area();
    f.render_widget(
        Block::new().style(Style::new().bg(app.theme.main_bg).fg(app.theme.main_fg)),
        area,
    );

    let areas = layout(area, app.show_viz, app.show_player, app.show_controls);
    let mut want_bars = 0;
    if let Some(r) = areas.viz {
        want_bars = draw_viz(f, app, r);
    }
    if let Some(r) = areas.player {
        draw_player(f, app, r);
    }
    if let Some(r) = areas.controls {
        draw_controls(f, app, r);
    }
    if areas.viz.is_none() && areas.player.is_none() && areas.controls.is_none() {
        let hint = Paragraph::new("all boxes hidden — press 1, 2 or 3")
            .alignment(Alignment::Center)
            .style(Style::new().fg(app.theme.inactive_fg));
        f.render_widget(
            hint,
            Rect {
                y: area.height / 2,
                height: 1,
                ..area
            },
        );
    }
    let error = app.flash.as_ref().is_some_and(|f| f.error);
    if app.overlay.is_some() || error {
        // Overlays cut through kitty image rows; re-anchor the cells beside them.
        let anchors = kitty_fix::Anchors::capture(f.buffer_mut());
        overlay::draw(f, app);
        if error {
            draw_error(f, app);
        }
        if !anchors.is_empty() {
            anchors.repair(f.buffer_mut());
        }
    }
    want_bars
}

/// Error box near the bottom of the screen: the full message, wrapped, so
/// nothing is cut off. Esc or a click closes it; it also fades on its own.
fn draw_error(f: &mut Frame, app: &mut App) {
    let Some(flash) = app.flash.as_ref().filter(|f| f.error) else {
        return;
    };
    let err = app.theme.error;
    let area = f.area();
    if area.width < 20 || area.height < 5 {
        return;
    }
    let width = area.width.saturating_sub(4).min(100);
    let text_w = width.saturating_sub(4).max(1) as usize;
    let lines = wrap_text(&flash.text, text_w);
    let height = (lines.len() as u16 + 2).min(area.height.saturating_sub(2));
    let r = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + area.height.saturating_sub(height + 1),
        width,
        height,
    );
    let t = &app.theme;
    let block = Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(err))
        .style(Style::new().bg(t.main_bg).fg(t.main_fg))
        .title(Line::from(vec![
            Span::styled("─┐", Style::new().fg(err)),
            Span::styled("error", Style::new().fg(err).add_modifier(Modifier::BOLD)),
            Span::styled("┌", Style::new().fg(err)),
        ]))
        .title_bottom(
            Line::from(vec![
                Span::styled("┐", Style::new().fg(err)),
                Span::styled("esc closes", Style::new().fg(t.inactive_fg)),
                Span::styled("┌─", Style::new().fg(err)),
            ])
            .alignment(Alignment::Right),
        );
    let inner = block.inner(r);
    f.render_widget(ratatui::widgets::Clear, r);
    f.render_widget(block, r);
    let inner = Rect {
        x: inner.x + 1,
        width: inner.width.saturating_sub(2),
        ..inner
    };
    let body: Vec<Line> = lines.into_iter().map(Line::from).collect();
    f.render_widget(Paragraph::new(body).style(Style::new().fg(t.main_fg)), inner);
    app.hits.add(r, Hit::CloseError);
}

/// Greedy word wrap that never drops text (long words are split).
pub fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out = Vec::new();
    for para in text.lines() {
        let mut line = String::new();
        for word in para.split_whitespace() {
            let mut word = word.to_string();
            loop {
                let len = line.chars().count();
                let wlen = word.chars().count();
                let sep = usize::from(len > 0);
                if len + sep + wlen <= width {
                    if sep == 1 {
                        line.push(' ');
                    }
                    line.push_str(&word);
                    break;
                }
                if len > 0 {
                    out.push(std::mem::take(&mut line));
                    continue;
                }
                // A single word longer than the line: split it.
                let head: String = word.chars().take(width).collect();
                word = word.chars().skip(width).collect();
                out.push(head);
                if word.is_empty() {
                    break;
                }
            }
        }
        out.push(line);
    }
    while out.last().is_some_and(String::is_empty) && out.len() > 1 {
        out.pop();
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

fn draw_viz(f: &mut Frame, app: &mut App, area: Rect) -> usize {
    let t = &app.theme;
    let mut title = vec![Span::styled("─", Style::new().fg(t.box_viz))];
    title.extend(border_label(t, t.box_viz, Some(1), "visualizer", true));
    let mode = format!("v {}", app.viz_mode.label());
    let mut right = border_label(t, t.box_viz, None, "", false);
    right.insert(1, Span::styled(mode, Style::new().fg(t.graph_text)));
    right.push(Span::styled("─", Style::new().fg(t.box_viz)));
    let right_w = spans_width(&right);
    if area.width > right_w + 2 {
        let rx = area.x + area.width - 1 - right_w;
        app.hits.add(
            Rect::new(rx, area.y, right_w.saturating_sub(1), 1),
            Hit::Action(Action::VizMode),
        );
    }
    let status = if app.viz_frame.active {
        Span::styled("● live", Style::new().fg(t.accent))
    } else if app.player.paused && app.current().is_some() {
        Span::styled("paused", Style::new().fg(t.inactive_fg))
    } else {
        Span::styled("waiting for audio", Style::new().fg(t.inactive_fg))
    };
    let mut bottom = vec![Span::styled("─┐", Style::new().fg(t.box_viz)), status];
    bottom.push(Span::styled("┌", Style::new().fg(t.box_viz)));

    let block = boxed(app, t.box_viz)
        .title(Line::from(title))
        .title(Line::from(right).alignment(Alignment::Right))
        .title_bottom(Line::from(bottom));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let inner = Rect {
        x: inner.x + 1,
        width: inner.width.saturating_sub(2),
        ..inner
    };
    let (bw, sp) = (app.cfg.viz.bar_width, app.cfg.viz.bar_spacing);
    f.render_widget(
        VizWidget {
            frame: &app.viz_frame,
            mode: app.viz_mode,
            theme: &app.theme,
            bar_width: bw,
            spacing: sp,
        },
        inner,
    );
    bars_needed(inner.width, app.viz_mode, bw, sp)
}

fn draw_player(f: &mut Frame, app: &mut App, area: Rect) {
    let t = app.theme.clone();
    let border = t.box_player;
    let mut title = vec![
        Span::styled("─", Style::new().fg(border)),
        Span::styled(superscript(2), Style::new().fg(t.hi_fg)),
    ];
    let mut x = area.x + 1 + spans_width(&title);
    for tab in Tab::ALL {
        let (label, key_at) = tab.label();
        let spans = tab_label(&t, border, label, key_at, tab == app.tab);
        let w = spans_width(&spans);
        if x + w < area.x + area.width.saturating_sub(1) {
            app.hits.add(Rect::new(x, area.y, w, 1), Hit::Tab(tab));
        }
        x += w;
        title.extend(spans);
    }

    let login = if app.logged_in {
        let label = match app.account.as_ref().and_then(|a| a.name.as_deref()) {
            Some(name) => format!("● {}", tabs::ellipsize(name, 20)),
            None => "● signed in".into(),
        };
        Span::styled(label, Style::new().fg(t.accent))
    } else {
        Span::styled("○ signed out", Style::new().fg(t.inactive_fg))
    };
    let right = vec![
        Span::styled("┐", Style::new().fg(border)),
        login,
        Span::styled("┌─", Style::new().fg(border)),
    ];
    let right_w = spans_width(&right);
    if area.width > right_w + 2 && area.height >= 2 {
        let rx = area.x + area.width - 1 - right_w;
        app.hits
            .add(Rect::new(rx, area.y + area.height - 1, right_w, 1), Hit::Auth);
    }
    let mut block = boxed(app, border)
        .title(Line::from(title))
        .title_bottom(Line::from(right).alignment(Alignment::Right));
    // Errors get their own box (draw_error) so the whole message is readable.
    if let Some(flash) = app.flash.as_ref().filter(|f| !f.error) {
        block = block.title_bottom(Line::from(vec![
            Span::styled("─┐", Style::new().fg(border)),
            Span::styled(
                flash.text.clone(),
                Style::new().fg(t.accent).add_modifier(Modifier::BOLD),
            ),
            Span::styled("┌", Style::new().fg(border)),
        ]));
    }
    let inner = block.inner(area);
    f.render_widget(block, area);
    let inner = Rect {
        x: inner.x + 1,
        width: inner.width.saturating_sub(2),
        ..inner
    };
    match app.tab {
        Tab::NowPlaying => tabs::now_playing(f, app, inner),
        Tab::Queue => tabs::queue(f, app, inner),
        Tab::Lyrics => tabs::lyrics(f, app, inner),
        Tab::Search => tabs::search(f, app, inner),
        Tab::Library => tabs::library(f, app, inner),
    }
}

/// `●━━━━━━○──────` progress line with the theme gradient.
pub fn progress_spans(theme: &Theme, width: u16, frac: f64) -> Vec<Span<'static>> {
    let w = width.max(2) as usize;
    let knob = ((frac.clamp(0.0, 1.0)) * (w - 1) as f64).round() as usize;
    let mut spans = Vec::with_capacity(w);
    for i in 0..w {
        let c = theme.progress_color(i as f32 / (w - 1) as f32);
        let s = if i < knob {
            Span::styled("━", Style::new().fg(c))
        } else if i == knob {
            Span::styled("●", Style::new().fg(c).add_modifier(Modifier::BOLD))
        } else {
            Span::styled("─", Style::new().fg(theme.meter_bg))
        };
        spans.push(s);
    }
    spans
}

fn draw_controls(f: &mut Frame, app: &mut App, area: Rect) {
    let t = app.theme.clone();
    let border = t.box_controls;
    let mut title = vec![Span::styled("─", Style::new().fg(border))];
    title.extend(border_label(&t, border, Some(3), "controls", true));
    let block = boxed(app, border).title(Line::from(title));
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height == 0 {
        return;
    }

    let on = Style::new().fg(t.title).add_modifier(Modifier::BOLD);
    let off = Style::new().fg(t.inactive_fg);
    let playing = !app.player.paused && !app.player.idle;
    let shuffle = if app.queue.shuffle { on.fg(t.accent) } else { off };
    let repeat_label = match app.queue.repeat {
        crate::player::queue::Repeat::Off => "↻",
        crate::player::queue::Repeat::All => "↻ all",
        crate::player::queue::Repeat::One => "↻ one",
    };
    let repeat = if app.queue.repeat != crate::player::queue::Repeat::Off {
        on.fg(t.accent)
    } else {
        off
    };
    let buttons: Vec<(String, Style, Action)> = vec![
        ("⇄".into(), shuffle, Action::Shuffle),
        ("⏮".into(), on, Action::Prev),
        (
            (if playing { "⏸" } else { "▶" }).into(),
            on.fg(t.hi_fg),
            Action::PlayPause,
        ),
        ("⏭".into(), on, Action::Next),
        (repeat_label.into(), repeat, Action::Repeat),
    ];
    let gap = 4u16;
    let total: u16 = buttons
        .iter()
        .map(|(s, ..)| Span::raw(s.as_str()).width() as u16 + 2)
        .sum::<u16>()
        + gap * (buttons.len() as u16 - 1);
    let mut x = inner.x + inner.width.saturating_sub(total) / 2;
    let row = inner.y;
    let mut spans = Vec::new();
    let pad = inner.width.saturating_sub(total) / 2;
    spans.push(Span::raw(" ".repeat(pad as usize)));
    for (i, (label, style, action)) in buttons.into_iter().enumerate() {
        let text = format!(" {label} ");
        let w = Span::raw(text.as_str()).width() as u16;
        app.hits.add(Rect::new(x, row, w, 1), Hit::Action(action));
        x += w + gap;
        spans.push(Span::styled(text, style));
        if i < 4 {
            spans.push(Span::raw(" ".repeat(gap as usize)));
        }
    }
    f.render_widget(Paragraph::new(Line::from(spans)), Rect { height: 1, ..inner });

    if inner.height >= 2 {
        // Volume meter, btop style.
        let vol = app.player.volume.clamp(0.0, 100.0);
        let label = if app.player.muted {
            "muted".to_string()
        } else {
            format!("{:>3.0}%", vol)
        };
        let meter_w = inner.width.saturating_sub(16).min(40);
        let filled = ((vol / 100.0) * meter_w as f64).round() as u16;
        let mut v = vec![Span::styled("vol ", Style::new().fg(t.graph_text))];
        for i in 0..meter_w {
            let c = if i < filled && !app.player.muted {
                Span::styled("■", Style::new().fg(t.progress_color(i as f32 / meter_w.max(1) as f32)))
            } else {
                Span::styled("■", Style::new().fg(t.meter_bg))
            };
            v.push(c);
        }
        v.push(Span::styled(format!(" {label}"), Style::new().fg(t.main_fg)));
        let line_w = spans_width(&v);
        let vx = inner.x + inner.width.saturating_sub(line_w) / 2;
        let vy = inner.y + 1;
        f.render_widget(
            Paragraph::new(Line::from(v)),
            Rect::new(vx, vy, line_w.min(inner.width), 1),
        );
        // Click "vol" or the %/muted label to mute; click or scroll the meter to set volume.
        let vol_label_w = 4u16; // "vol "
        if vx + vol_label_w <= inner.x + inner.width {
            app.hits
                .add(Rect::new(vx, vy, vol_label_w, 1), Hit::Action(Action::Mute));
        }
        let meter_x = vx + vol_label_w;
        if meter_w > 1 && meter_x + meter_w <= inner.x + inner.width {
            app.hits.add(Rect::new(meter_x, vy, meter_w, 1), Hit::Volume);
        }
        let pct_x = meter_x + meter_w;
        let pct_w = (label.len() as u16 + 1).min((inner.x + inner.width).saturating_sub(pct_x));
        if pct_w > 0 {
            app.hits.add(Rect::new(pct_x, vy, pct_w, 1), Hit::Action(Action::Mute));
        }
    }

    if inner.height >= 3 {
        let time = if app.current().is_some() {
            format!(
                "{} / {}",
                format_time(app.player.time),
                format_time(app.player.duration)
            )
        } else {
            String::new()
        };
        let k = |a| app.keymap.hint(a);
        let hints = format!(
            "{time}   {} play  {}/{} track  {}/{} seek  {} help",
            k(Action::PlayPause),
            k(Action::Prev),
            k(Action::Next),
            k(Action::SeekBackward),
            k(Action::SeekForward),
            k(Action::Help)
        );
        f.render_widget(
            Paragraph::new(hints)
                .alignment(Alignment::Center)
                .style(Style::new().fg(t.inactive_fg)),
            Rect {
                y: inner.y + 2,
                height: 1,
                ..inner
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use tokio::sync::mpsc::unbounded_channel;

    use super::*;
    use crate::app::{Channels, Overlay};
    use crate::config::{Config, Paths, State};
    use crate::keymap::Keymap;
    use crate::lyrics::Lyrics;
    use crate::model::{AlbumRef, Item, Track};

    #[test]
    fn menu_toggles_and_up_next_render() {
        let screen = |app: &mut App| {
            let mut term = Terminal::new(TestBackend::new(150, 42)).unwrap();
            term.draw(|f| {
                draw(f, app);
            })
            .unwrap();
            let s: String = term.backend().buffer().content().iter().map(|c| c.symbol()).collect();
            s
        };
        let mut app = app();
        // Queue is on track 3, so track 4 is up next.
        let np = screen(&mut app);
        assert!(np.contains("up next"), "no up-next line");
        assert!(np.contains("number 4"), "up next shows the wrong song");

        app.overlay = Some(Overlay::Menu {
            state: Default::default(),
        });
        let menu = screen(&mut app);
        assert!(menu.contains("Even loudness") && menu.contains("Notifications"));
    }

    fn track(i: usize) -> Track {
        Track {
            video_id: format!("vid{i}"),
            title: format!("A fairly long song title number {i} (feat. Somebody Else)"),
            artists: "Some Artist, Another Artist".into(),
            album: Some("An Album".into()),
            album_id: None,
            duration: Some(200.0 + i as f64),
            thumbnail: None,
            liked: Some(i.is_multiple_of(2)),
        }
    }

    fn app() -> App {
        let (p, _) = unbounded_channel();
        let (y, _) = unbounded_channel();
        let (l, _) = unbounded_channel();
        let (a, _) = unbounded_channel();
        let tmp = std::env::temp_dir().join("pova-ui-test");
        let paths = Paths {
            config_dir: tmp.clone(),
            cache_dir: tmp.clone(),
            state_dir: tmp.clone(),
            runtime_dir: tmp,
        };
        let (keymap, _) = Keymap::new(&Default::default());
        let mut app = App::new(
            Config::default(),
            paths,
            State::default(),
            keymap,
            Channels {
                player: p,
                ytm: y,
                lyrics: l,
                art: a,
                resolve: unbounded_channel().0,
                notify: unbounded_channel().0,
            },
            None,
            None,
        );
        app.queue.replace((0..30).map(track).collect(), 3);
        app.player.time = 42.0;
        app.player.duration = 203.0;
        app.player.idle = false;
        app.lyrics = crate::app::LyricsState::Found {
            lyrics: Lyrics::Synced((0..40).map(|i| (i as f64 * 3.0, format!("lyric line {i}"))).collect()),
            source: "LRCLIB",
        };
        app.viz_frame.bars = (0..40).map(|i| i as f64 / 40.0).collect();
        app.viz_frame.wave = (0..512).map(|i| ((i as f32) / 20.0).sin()).collect();
        app.viz_frame.active = true;
        app
    }

    /// Render every tab, overlay and visualizer mode at several sizes: must not panic.
    #[test]
    fn renders_everywhere() {
        let mut app = app();
        app.search.stack.push({
            let mut lv = crate::app::tests_support::list_view();
            lv.items = vec![
                Item::Header("Top songs".into()),
                Item::Track(track(1)),
                Item::Album(AlbumRef {
                    id: "x".into(),
                    title: "Album".into(),
                    subtitle: "2020".into(),
                }),
            ];
            lv
        });
        // Exercise the autocomplete dropdown too.
        app.search.editing = true;
        app.search.input = "call me".into();
        app.search.cursor = 7;
        app.search.suggest_for = "call me".into();
        app.search.suggestions = vec![
            crate::ytm::Suggestion {
                text: "call me when you're sober".into(),
                history: true,
            },
            crate::ytm::Suggestion {
                text: "call me maybe".into(),
                history: false,
            },
        ];
        app.search.suggest_sel = Some(1);
        for (w, h) in [(20, 6), (60, 20), (89, 30), (120, 40), (250, 70)] {
            let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
            for mode in [VizMode::Bars, VizMode::Mirror, VizMode::Mono, VizMode::Wave] {
                app.viz_mode = mode;
                for tab in Tab::ALL {
                    app.tab = tab;
                    for (v, p, c) in [
                        (true, true, true),
                        (false, true, true),
                        (true, false, true),
                        (true, true, false),
                        (false, false, false),
                    ] {
                        app.show_viz = v;
                        app.show_player = p;
                        app.show_controls = c;
                        term.draw(|f| {
                            draw(f, &mut app);
                        })
                        .unwrap();
                    }
                }
            }
            app.show_viz = true;
            app.show_player = true;
            app.show_controls = true;
            for overlay in 0..3 {
                app.overlay = Some(match overlay {
                    0 => Overlay::Help { scroll: 5 },
                    1 => Overlay::Menu {
                        state: Default::default(),
                    },
                    _ => Overlay::Themes {
                        names: vec![("Default".into(), None)],
                        state: Default::default(),
                        original: app.theme.clone(),
                    },
                });
                term.draw(|f| {
                    draw(f, &mut app);
                })
                .unwrap();
            }
            app.overlay = None;
            // "No lyrics" view with the cat mascot (kitty protocol forced).
            app.tab = Tab::Lyrics;
            app.lyrics = crate::app::LyricsState::Missing;
            let mut picker = ratatui_image::picker::Picker::halfblocks();
            picker.set_protocol_type(ratatui_image::picker::ProtocolType::Kitty);
            let img = image::load_from_memory(include_bytes!("../../assets/mascot-sad-cat-tui.png")).unwrap();
            app.cat_art = Some(ratatui_image::thread::ThreadProtocol::new(
                unbounded_channel().0,
                Some(picker.new_resize_protocol(img)),
            ));
            term.draw(|f| {
                draw(f, &mut app);
            })
            .unwrap();
            app.cat_art = None;
            term.draw(|f| {
                draw(f, &mut app);
            })
            .unwrap();
        }
    }

    /// kitty draws a placeholder cell only if it carries its position or its
    /// left neighbour is part of the same image. With the Esc menu open over
    /// the album art, every image run must therefore start with a full anchor.
    #[test]
    fn overlay_keeps_kitty_art_beside_it() {
        use ratatui_image::picker::{Picker, ProtocolType};
        use ratatui_image::thread::ThreadProtocol;
        const PH: char = '\u{10EEEE}';

        let mut picker = Picker::halfblocks();
        picker.set_protocol_type(ProtocolType::Kitty);
        let (tx, mut rx) = unbounded_channel();
        let mut app = app();
        app.tab = Tab::NowPlaying;
        app.picker = Some(picker.clone());
        let mut proto = ThreadProtocol::new(tx, None);
        let img = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(600, 600, image::Rgb([30, 140, 220])));
        proto.replace_protocol(picker.new_resize_protocol(img));
        app.art = Some(proto);
        app.art_loading = false;
        // Same shape as the bug report: wide terminal, art top-right, menu centred.
        let mut term = Terminal::new(TestBackend::new(200, 50)).unwrap();
        term.draw(|f| {
            draw(f, &mut app);
        })
        .unwrap();
        let done = rx.try_recv().unwrap().resize_encode().unwrap();
        app.art.as_mut().unwrap().update_resized_protocol(done);
        app.overlay = Some(Overlay::Menu {
            state: Default::default(),
        });
        term.draw(|f| {
            draw(f, &mut app);
        })
        .unwrap();

        let buf = term.backend().buffer();
        let mut runs = 0;
        for y in 0..buf.area.height {
            let mut prev = false;
            for x in 0..buf.area.width {
                let sym = buf[(x, y)].symbol();
                let is_img = sym.contains(PH);
                if is_img && !prev {
                    runs += 1;
                    let marks = sym.split_once(PH).unwrap().1.chars().count();
                    assert!(marks >= 3, "image run at ({x},{y}) has no anchor: {sym:?}");
                }
                prev = is_img;
            }
        }
        assert!(runs > 0, "art not drawn");
    }

    #[test]
    fn esc_menu_shows_logo_banner() {
        let mut app = app();
        app.overlay = Some(Overlay::Menu {
            state: Default::default(),
        });
        let mut term = Terminal::new(TestBackend::new(150, 42)).unwrap();
        term.draw(|f| {
            draw(f, &mut app);
        })
        .unwrap();
        let buf = term.backend().buffer();
        let screen: String = buf.content().iter().map(|c| c.symbol()).collect();
        assert!(screen.contains("Autoplay") && screen.contains("Album art"));
        // Large banner (scale 2): its frame uses the logo's violet at the left edge.
        let violet = buf
            .content()
            .iter()
            .any(|c| c.fg == ratatui::style::Color::Rgb(0x6b, 0x1f, 0xfc));
        assert!(violet, "banner gradient missing");
        // At scale 2 every logo pixel is a full block.
        assert!(screen.matches('█').count() > 100);
    }

    #[test]
    fn wide_layout_matches_sketch() {
        let a = layout(Rect::new(0, 0, 150, 42), true, true, true);
        let (v, p, c) = (a.viz.unwrap(), a.player.unwrap(), a.controls.unwrap());
        assert!(v.width > p.width, "visualizer is the big box");
        assert_eq!(p.x, c.x);
        assert!(c.y > p.y && c.height == CONTROLS_HEIGHT);
        assert_eq!(v.height, 42);
    }

    use crate::config::VizMode;

    /// Album art through the kitty graphics protocol, using the same threaded
    /// resize path as the app: the first render requests a resize, the worker
    /// encodes, and the next render must emit kitty's transmit escape plus
    /// unicode placeholder cells.
    #[test]
    fn kitty_album_art_renders_placeholders() {
        use ratatui_image::picker::{Picker, ProtocolType};
        use ratatui_image::thread::ThreadProtocol;

        let mut picker = Picker::halfblocks();
        picker.set_protocol_type(ProtocolType::Kitty);
        let (tx, mut rx) = unbounded_channel();
        let mut app = app();
        app.tab = Tab::NowPlaying;
        app.picker = Some(picker.clone());
        let mut proto = ThreadProtocol::new(tx, None);
        let img = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(64, 64, image::Rgb([200, 30, 90])));
        proto.replace_protocol(picker.new_resize_protocol(img));
        app.art = Some(proto);
        app.art_loading = false;

        let mut term = Terminal::new(TestBackend::new(150, 42)).unwrap();
        term.draw(|f| {
            draw(f, &mut app);
        })
        .unwrap();
        // Worker side (the art-resize thread in main.rs).
        let req = rx.try_recv().expect("render should request a resize/encode");
        let done = req.resize_encode().expect("kitty encode");
        assert!(app.art.as_mut().unwrap().update_resized_protocol(done));
        term.draw(|f| {
            draw(f, &mut app);
        })
        .unwrap();

        let buf = term.backend().buffer();
        let symbols: Vec<&str> = buf.content().iter().map(|c| c.symbol()).collect();
        let placeholders = symbols.iter().filter(|s| s.contains('\u{10EEEE}')).count();
        let transmit = symbols.iter().any(|s| s.contains("\x1b_G"));
        assert!(transmit, "kitty transmit escape (ESC _G) missing");
        assert!(placeholders > 50, "only {placeholders} kitty placeholder cells");
    }

    #[test]
    fn wrap_keeps_every_word() {
        let text = "Couldn't like the song: YouTube Music signed you out, run pova-import-cookie chrome";
        let lines = wrap_text(text, 20);
        assert!(lines.iter().all(|l| l.chars().count() <= 20));
        assert_eq!(lines.join(" "), text);
        // A word longer than the line is split, not dropped.
        assert_eq!(wrap_text("abcdefghij", 4), vec!["abcd", "efgh", "ij"]);
        assert_eq!(wrap_text("", 10), vec![""]);
    }
}
