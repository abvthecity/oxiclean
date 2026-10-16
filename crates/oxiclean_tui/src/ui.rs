//! Drawing the program map.
//!
//! The type follows one scale: bold for the one thing each line is about,
//! the terminal's own color for content, muted for anything that explains
//! it, and muted bold capitals for section labels. Numbers are right
//! aligned in fixed columns so they line up down a list.

use std::time::{Duration, Instant};

use oxiclean_typecheck::program_map::{Cause, PackageChecks, Phase, ProgramMap};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Margin, Rect},
    style::Modifier,
    text::{Line, Span},
    widgets::Paragraph,
};

use crate::{
    app::{App, REVEAL, SELECT, Scan},
    theme::{
        ACCENT, ERROR, MUTED, Rgb, SPINNER, TRACK, Theme, WARN, bar, cause_color, compact, grouped,
    },
};

/// Draws the whole view.
pub(crate) fn render(f: &mut Frame<'_>, app: &mut App, theme: Theme, now: Instant) {
    let area = f.area().inner(Margin { horizontal: 2, vertical: 1 });
    if app.loaded.is_none() {
        match (&app.scan, &app.error) {
            (Some(scan), _) => loading(f, area, scan, theme, now),
            (None, Some(e)) => failed(f, area, e, theme),
            (None, None) => {}
        }
        return;
    }
    map_view(f, area, app, theme, now);
}

/// `t` eased out: fast at first, settling at the end.
fn ease(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

/// How far an animation of `length` from `start` has got at `now`.
fn progress(app: &App, start: Instant, length: Duration, now: Instant) -> f32 {
    if !app.motion {
        return 1.0;
    }
    ease((now - start).as_secs_f32() / length.as_secs_f32())
}

fn spinner(since: Instant, now: Instant) -> &'static str {
    let frame = (now - since).as_millis() / 80;
    SPINNER[usize::try_from(frame).unwrap_or(0) % SPINNER.len()]
}

/// Where the glint on a filling bar is, sweeping past the end and back to
/// the start every 1.4 seconds.
fn glint(since: Instant, now: Instant) -> f32 {
    let ms = (now - since).as_millis() % 1400;
    #[allow(clippy::cast_precision_loss)]
    let t = ms as f32 / 1400.0;
    t * 1.4 - 0.2
}

fn seconds(d: Duration) -> String {
    if d < Duration::from_secs(10) {
        format!("{:.2}s", d.as_secs_f32())
    } else {
        format!("{:.1}s", d.as_secs_f32())
    }
}

/// `s` in `width` characters, cut at the start: the end of a path says
/// the most.
fn fit_tail(s: &str, width: usize) -> String {
    let n = s.chars().count();
    if n <= width {
        return s.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let tail: String = s.chars().skip(n - (width - 1)).collect();
    format!("…{tail}")
}

/// `s` in `width` characters, cut at the end.
fn fit_head(s: &str, width: usize) -> String {
    if s.chars().count() <= width {
        return s.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let head: String = s.chars().take(width - 1).collect();
    format!("{head}…")
}

/// `n` and `noun`, with an `s` unless `n` is one: `1 file`, `1,204 files`.
fn plural(n: usize, noun: &str) -> String {
    format!("{} {noun}{}", grouped(n), if n == 1 { "" } else { "s" })
}

/// `s` padded or cut to exactly `width` characters.
fn pad(s: &str, width: usize) -> String {
    let s = fit_head(s, width);
    let n = s.chars().count();
    format!("{s}{}", " ".repeat(width - n))
}

/// `s` right aligned in `width` characters.
fn right(s: &str, width: usize) -> String {
    format!("{s:>width$}")
}

/// A line with `left` at the start and `right` at the end of `width`.
fn spread<'a>(mut left: Vec<Span<'a>>, right: Vec<Span<'a>>, width: usize) -> Line<'a> {
    let used: usize = left.iter().chain(&right).map(|s| s.content.chars().count()).sum();
    left.push(Span::raw(" ".repeat(width.saturating_sub(used).max(1))));
    left.extend(right);
    Line::from(left)
}

/// A bar where a band sweeps back and forth, for work of unknown length.
fn sweep(theme: Theme, width: usize, since: Instant, now: Instant) -> Vec<Span<'static>> {
    let t = (now - since).as_secs_f32() * 2.4;
    let center = (t.sin() + 1.0) / 2.0;
    #[allow(clippy::cast_precision_loss)]
    let span = (width as f32 * 0.22).max(3.0);
    (0..width)
        .map(|i| {
            #[allow(clippy::cast_precision_loss)]
            let x = i as f32;
            #[allow(clippy::cast_precision_loss)]
            let d = (x - center * (width.saturating_sub(1)) as f32).abs() / (span / 2.0);
            if d < 1.0 {
                Span::styled("━", theme.fg(TRACK.mix(ACCENT, 1.0 - d * d)))
            } else {
                Span::styled("─", theme.fg(TRACK))
            }
        })
        .collect()
}

/// The steps of a scan, each with its time once done, and a bar under
/// the one in progress.
fn scan_lines(scan: &Scan, theme: Theme, width: usize, now: Instant) -> Vec<Line<'static>> {
    let current = scan.progress.phase();
    let shown = [Phase::Workspace, Phase::Programs, Phase::Imports, Phase::Packages];
    let mut lines = Vec::new();
    let current_at = shown.iter().position(|p| *p == current).unwrap_or(shown.len());
    for (i, phase) in shown.iter().enumerate() {
        let started = scan.phases.iter().find(|(p, _)| p == phase).map(|(_, at)| *at);
        // A phase can finish between two frames; it is done all the same.
        let ended =
            scan.phases.iter().find(|(p, _)| shown[i + 1..].contains(p) || *p == Phase::Done);
        let (mark, label, detail) = if i < current_at {
            let took = match (started, ended) {
                (Some(s), Some((_, e))) => seconds(*e - s),
                _ => String::new(),
            };
            (Span::styled("✓", theme.accent()), theme.muted(), took)
        } else if i == current_at {
            let detail = if *phase == Phase::Imports {
                let (done, total) = scan.progress.files();
                format!("{} / {} files", grouped(done), grouped(total))
            } else {
                started.map(|s| seconds(now - s)).unwrap_or_default()
            };
            (Span::styled(spinner(scan.started, now), theme.accent()), theme.strong(), detail)
        } else {
            (Span::styled("·", theme.muted()), theme.muted(), String::new())
        };
        lines.push(spread(
            vec![mark, Span::raw("  "), Span::styled(phase.label(), label)],
            vec![Span::styled(detail, theme.muted())],
            width,
        ));
        if *phase == current {
            let w = width.saturating_sub(3);
            let mut spans = vec![Span::raw("   ")];
            if *phase == Phase::Imports {
                let (done, total) = scan.progress.files();
                #[allow(clippy::cast_precision_loss)]
                let frac = if total == 0 { 0.0 } else { done as f32 / total as f32 };
                let g = glint(scan.started, now);
                spans.extend(bar(theme, w, frac, ACCENT.mix(TRACK, 0.35), ACCENT, Some(g)));
            } else {
                spans.extend(sweep(theme, w, scan.started, now));
            }
            lines.push(Line::from(spans));
        }
    }
    lines
}

/// The first scan: its steps in a centered column.
fn loading(f: &mut Frame<'_>, area: Rect, scan: &Scan, theme: Theme, now: Instant) {
    let width = area.width.min(64);
    let height = 11.min(area.height);
    let x = area.x + (area.width - width) / 2;
    let y = area.y + (area.height - height) / 3;
    let column = Rect { x, y, width, height };
    let w = usize::from(width);
    let mut lines = vec![
        Line::from(vec![
            Span::styled("oxiclean", theme.accent().add_modifier(Modifier::BOLD)),
            Span::styled(" map", theme.strong()),
        ]),
        Line::from(Span::styled("Which tsconfig programs check each package", theme.muted())),
        Line::default(),
    ];
    lines.extend(scan_lines(scan, theme, w, now));
    lines.push(Line::default());
    lines.push(spread(
        vec![Span::styled(seconds(now - scan.started), theme.muted())],
        vec![Span::styled("q", theme.strong()), Span::styled(" quit", theme.muted())],
        w,
    ));
    f.render_widget(Paragraph::new(lines), column);
}

fn failed(f: &mut Frame<'_>, area: Rect, error: &str, theme: Theme) {
    let width = area.width.min(72);
    let column = Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + area.height / 3,
        width,
        height: area.height / 2,
    };
    let lines = vec![
        Line::from(Span::styled("The scan failed", theme.fg(ERROR).add_modifier(Modifier::BOLD))),
        Line::from(Span::styled(error.to_string(), theme.text())),
        Line::default(),
        Line::from(vec![
            Span::styled("r", theme.strong()),
            Span::styled(" try again   ", theme.muted()),
            Span::styled("q", theme.strong()),
            Span::styled(" quit", theme.muted()),
        ]),
    ];
    f.render_widget(Paragraph::new(lines).wrap(ratatui::widgets::Wrap { trim: false }), column);
}

/// The share of checks that repeat, colored by how bad it is.
fn share_color(share: f32) -> Rgb {
    if share >= 0.4 {
        ERROR
    } else if share >= 0.15 {
        WARN
    } else {
        ACCENT
    }
}

fn map_view(f: &mut Frame<'_>, area: Rect, app: &mut App, theme: Theme, now: Instant) {
    let loaded = app.loaded.as_ref().expect("map_view needs a loaded map");
    let map = &loaded.map;
    let reveal = progress(app, loaded.at, REVEAL, now);
    let causes = map.causes.len();
    let [header, _, summary, _, causes_area, body, _, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(2),
        Constraint::Length(1),
        Constraint::Length(if causes == 0 { 0 } else { u16::try_from(causes).unwrap_or(5) + 2 }),
        Constraint::Min(4),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(area);
    let w = usize::from(area.width);

    // Header: what this is, and what was scanned.
    let root = map.root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let packages = map.packages.len();
    let facts = match &app.scan {
        Some(scan) => {
            let (done, total) = scan.progress.files();
            let mut s =
                vec![Span::styled(format!("{} ", spinner(scan.started, now)), theme.accent())];
            s.push(Span::styled(
                format!("rescanning  {} / {} files", grouped(done), grouped(total)),
                theme.muted(),
            ));
            s
        }
        None => vec![Span::styled(
            format!(
                "{root}  ·  {packages} packages  ·  {} programs  ·  {}",
                map.programs,
                seconds(loaded.took)
            ),
            theme.muted(),
        )],
    };
    f.render_widget(
        spread(
            vec![
                Span::styled("oxiclean", theme.accent().add_modifier(Modifier::BOLD)),
                Span::styled(" map", theme.strong()),
            ],
            facts,
            w,
        ),
        header,
    );

    // Summary: the one number that matters, then where it comes from.
    #[allow(clippy::cast_precision_loss)]
    let share = if map.checks == 0 { 0.0 } else { map.repeats as f32 / map.checks as f32 };
    let lines = if map.checks == 0 {
        vec![Line::from(Span::styled("No tsconfig programs found under the root.", theme.muted()))]
    } else if map.repeats == 0 {
        vec![
            Line::from(Span::styled(
                "No repeated checks",
                theme.accent().add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                format!(
                    "Each of {} file checks across {} programs is done once.",
                    grouped(map.checks),
                    map.programs
                ),
                theme.muted(),
            )),
        ]
    } else {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let pct = (share * 100.0 * reveal).round() as u32;
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss
        )]
        let counted = |n: usize| (n as f32 * reveal).round() as usize;
        let big = format!("{pct}%");
        let indent = " ".repeat(big.chars().count() + 2);
        vec![
            Line::from(vec![
                Span::styled(big, theme.fg(share_color(share)).add_modifier(Modifier::BOLD)),
                Span::raw("  "),
                Span::styled(
                    "of type checking repeats work another program already did",
                    theme.text(),
                ),
            ]),
            Line::from(vec![
                Span::raw(indent),
                Span::styled(
                    format!(
                        "{} of {} file checks across {} programs",
                        grouped(counted(map.repeats)),
                        grouped(counted(map.checks)),
                        map.programs
                    ),
                    theme.muted(),
                ),
            ]),
        ]
    };
    f.render_widget(Paragraph::new(lines), summary);

    if causes > 0 {
        render_causes(f, causes_area, app, map, theme, reveal);
    }

    if usize::from(body.width) >= 100 {
        let [list, _, detail] = Layout::horizontal([
            Constraint::Percentage(44),
            Constraint::Length(4),
            Constraint::Fill(1),
        ])
        .areas(body);
        render_list(f, list, app, theme, reveal);
        render_detail(f, detail, app, theme, now);
    } else {
        let [list, detail] =
            Layout::vertical([Constraint::Percentage(50), Constraint::Fill(1)]).areas(body);
        render_list(f, list, app, theme, reveal);
        render_detail(f, detail, app, theme, now);
    }
    render_footer(f, footer, app, theme, now);
}

fn render_causes(
    f: &mut Frame<'_>,
    area: Rect,
    app: &App,
    map: &ProgramMap,
    theme: Theme,
    reveal: f32,
) {
    let w = usize::from(area.width);
    let mut lines = vec![Line::from(vec![
        Span::styled("CAUSES", theme.label()),
        Span::styled(format!("   1–{} filter   0 all", map.causes.len()), theme.muted()),
    ])];
    // key, tag, description, bar, share, count, with two spaces between.
    let (tag_w, share_w, count_w) = (8, 4, 6);
    // Marker and key, tag, gaps, then the share and count columns.
    let fixed = 5 + tag_w + 2 + 2 + (share_w + 2) + (count_w + 2);
    let longest = map.causes.iter().map(|c| c.cause.describe().chars().count()).max().unwrap_or(0);
    let bar_w = (w.saturating_sub(fixed + longest)).clamp(8, 28);
    let desc_w = longest.min(w.saturating_sub(fixed + bar_w));
    for (i, c) in map.causes.iter().enumerate() {
        let (cause, n) = (&c.cause, &c.repeats);
        let on = app.cause == Some(*cause);
        let dimmed = app.cause.is_some() && !on;
        let color = cause_color(*cause);
        #[allow(clippy::cast_precision_loss)]
        let frac = *n as f32 / map.repeats.max(1) as f32;
        let tag_style =
            if dimmed { theme.muted() } else { theme.fg(color).add_modifier(Modifier::BOLD) };
        let desc_style = if on {
            theme.strong()
        } else if dimmed {
            theme.muted()
        } else {
            theme.text()
        };
        let mut spans = vec![
            Span::styled(if on { "› " } else { "  " }, theme.accent()),
            Span::styled(format!("{}  ", i + 1), theme.muted()),
            Span::styled(pad(cause.tag(), tag_w), tag_style),
            Span::raw("  "),
            Span::styled(pad(cause.describe(), desc_w), desc_style),
            Span::raw("  "),
        ];
        let (from, to) = if dimmed { (TRACK, MUTED) } else { (color.mix(TRACK, 0.45), color) };
        spans.extend(bar(theme, bar_w, frac * reveal, from, to, None));
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let pct = (frac * 100.0).round() as u32;
        spans.push(Span::styled(right(&format!("{pct}%"), share_w + 2), theme.text()));
        spans.push(Span::styled(right(&compact(*n), count_w + 2), theme.muted()));
        lines.push(Line::from(spans));
    }
    f.render_widget(Paragraph::new(lines), area);
}

fn render_list(f: &mut Frame<'_>, area: Rect, app: &mut App, theme: Theme, reveal: f32) {
    let w = usize::from(area.width);
    let (hidden, once) = app.hidden();
    let mut title = vec![
        Span::styled("PACKAGES", theme.label()),
        Span::styled(format!("   {} with repeats", app.visible.len()), theme.muted()),
    ];
    if !app.search.is_empty() {
        title.push(Span::styled(format!("   /{}", app.search), theme.accent()));
    }
    let header = spread(
        title,
        vec![Span::styled(
            format!("{}{}", right("programs", 9), right("repeated", 22)),
            theme.muted(),
        )],
        w,
    );
    let mut lines = vec![header];

    let rows = usize::from(area.height).saturating_sub(3);
    if app.selected < app.offset {
        app.offset = app.selected;
    } else if rows > 0 && app.selected >= app.offset + rows {
        app.offset = app.selected + 1 - rows;
    }
    let Some(map) = app.map() else { return };
    if app.visible.is_empty() {
        let message = if app.search.is_empty() && app.cause.is_none() {
            "Every package is checked by one program."
        } else {
            "No package matches. Esc clears the filter."
        };
        lines.push(Line::from(Span::styled(message, theme.muted())));
    }
    let max = app.visible.first().map_or(1, |&i| app.weight(&map.packages[i]).max(1));
    let (count_w, bar_w, num_w) = (9, 12, 8);
    let name_w = w.saturating_sub(2 + count_w + 2 + bar_w + num_w);
    for (row, &i) in app.visible.iter().enumerate().skip(app.offset).take(rows) {
        let p = &map.packages[i];
        let selected = row == app.selected;
        let n = p.checked_by();
        let n_color = if n > 10 {
            ERROR
        } else if n > 3 {
            WARN
        } else {
            MUTED
        };
        let weight = app.weight(p);
        #[allow(clippy::cast_precision_loss)]
        let frac = weight as f32 / max as f32;
        let color = app.cause.map_or(ERROR, cause_color);
        let mut spans = vec![
            Span::styled(if selected { "› " } else { "  " }, theme.accent()),
            Span::styled(
                pad(&p.name, name_w),
                if selected { theme.strong() } else { theme.text() },
            ),
            Span::styled(right(&format!("{n}×"), count_w), theme.fg(n_color)),
            Span::raw("  "),
        ];
        spans.extend(bar(theme, bar_w, frac * reveal, color.mix(TRACK, 0.5), color, None));
        spans.push(Span::styled(right(&compact(weight), num_w), theme.text()));
        lines.push(Line::from(spans));
    }
    let shown = app.visible.len().saturating_sub(app.offset).min(rows);
    if shown < rows || app.visible.len() <= rows {
        let mut rest = Vec::new();
        if hidden > 0 {
            rest.push(format!("{hidden} filtered out"));
        }
        if once > 0 {
            rest.push(format!("{once} checked once"));
        }
        if !rest.is_empty() {
            lines.push(Line::from(Span::styled(format!("  + {}", rest.join(", ")), theme.muted())));
        }
    } else {
        let below = app.visible.len() - app.offset - shown;
        lines.push(Line::from(Span::styled(
            format!(
                "  {} more below{}",
                below,
                if once > 0 { format!(", {once} checked once") } else { String::new() }
            ),
            theme.muted(),
        )));
    }
    f.render_widget(Paragraph::new(lines), area);
}

/// The programs that check the selected package, grouped by how they
/// reach it.
fn render_detail(f: &mut Frame<'_>, area: Rect, app: &App, theme: Theme, now: Instant) {
    let Some(p) = app.package() else { return };
    let grow = progress(app, app.selected_at, SELECT, now);
    let w = usize::from(area.width);
    let h = usize::from(area.height);
    let mut lines: Vec<Line<'_>> = vec![
        Line::from(vec![
            Span::styled(fit_head(&p.name, w.saturating_sub(2)), theme.strong()),
            Span::raw("  "),
            Span::styled(
                fit_tail(&p.dir, w.saturating_sub(p.name.chars().count() + 2)),
                theme.muted(),
            ),
        ]),
        Line::from(Span::styled(
            format!(
                "{}  ·  checked by {}  ·  {}",
                plural(p.files, "file"),
                plural(p.checked_by(), "program"),
                plural(p.repeats, "repeated check"),
            ),
            theme.muted(),
        )),
        Line::default(),
    ];
    if let Some((top, n)) = top_cause(p, app.cause) {
        #[allow(clippy::cast_precision_loss)]
        let share = (n as f32 / p.repeats.max(1) as f32 * 100.0).round();
        lines.push(Line::from(vec![
            Span::styled("→ ", theme.accent()),
            Span::styled(fit_head(top.fix(), w.saturating_sub(2)), theme.text()),
        ]));
        lines.push(Line::from(Span::styled(
            format!("  removes {} here, {share}% of this package's repeats", plural(n, "check")),
            theme.muted(),
        )));
        lines.push(Line::default());
    }

    // The repeats first, biggest first, then the checks that are needed.
    let mut order: Vec<Cause> = Cause::REPEATS.to_vec();
    order.sort_by_key(|&c| std::cmp::Reverse(p.repeats_of(c)));
    order.extend([Cause::Owner, Cause::Variant]);
    let (bar_w, files_w) = (8, 7);
    let longest = p.checkers.iter().map(|c| c.program.chars().count()).max().unwrap_or(0);
    let prog_w = longest.min((w.saturating_sub(2 + 2 + bar_w + files_w) / 2).max(12));
    for cause in order {
        let members: Vec<_> = p.checkers.iter().filter(|c| c.cause == cause).collect();
        if members.is_empty() || lines.len() + 2 > h {
            continue;
        }
        let total: usize = members.iter().map(|c| c.repeats).sum();
        let color = cause_color(cause);
        let label = plural(members.len(), "program");
        lines.push(spread(
            vec![
                Span::styled(
                    cause.tag().to_uppercase(),
                    theme.fg(color).add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!("   {label}"), theme.muted()),
            ],
            vec![Span::styled(
                if total > 0 {
                    format!("{} repeated", plural(total, "check"))
                } else {
                    String::new()
                },
                theme.muted(),
            )],
            w,
        ));
        // Up to five programs a group, fewer when the pane runs out, and
        // a line for the rest.
        let room = h.saturating_sub(lines.len() + 1);
        let mut limit = members.len().min(5).min(room);
        if limit < members.len() {
            limit = limit.min(room.saturating_sub(1)).max(1);
        }
        for c in members.iter().take(limit) {
            #[allow(clippy::cast_precision_loss)]
            let frac = c.files as f32 / p.files.max(c.files).max(1) as f32;
            let mut spans = vec![
                Span::raw("  "),
                Span::styled(pad(&fit_tail(&c.program, prog_w), prog_w), theme.text()),
                Span::raw("  "),
            ];
            spans.extend(bar(theme, bar_w, frac * grow, color.mix(TRACK, 0.5), color, None));
            spans.push(Span::styled(right(&grouped(c.files), files_w), theme.text()));
            if let Some(via) = c.via.as_ref().filter(|_| c.repeats > 0) {
                // An import says where it is; an include, one file it takes.
                let via = if c.cause == Cause::SourceImport {
                    via.clone()
                } else {
                    format!("e.g. {via}")
                };
                let room = w.saturating_sub(2 + prog_w + 2 + bar_w + files_w + 3);
                if room > 8 {
                    spans.push(Span::styled(format!("   {}", fit_tail(&via, room)), theme.muted()));
                }
            }
            lines.push(Line::from(spans));
        }
        if members.len() > limit {
            lines.push(Line::from(Span::styled(
                format!("  … {} more", members.len() - limit),
                theme.muted(),
            )));
        }
        lines.push(Line::default());
    }
    f.render_widget(Paragraph::new(lines), area);
}

/// The cause behind most of `p`'s repeats, or `filter` when one is set,
/// with how many checks it makes.
fn top_cause(p: &PackageChecks, filter: Option<Cause>) -> Option<(Cause, usize)> {
    let pick = |c: Cause| (c, p.repeats_of(c));
    match filter {
        Some(c) => Some(pick(c)).filter(|(_, n)| *n > 0),
        None => {
            Cause::REPEATS.into_iter().map(pick).filter(|(_, n)| *n > 0).max_by_key(|(_, n)| *n)
        }
    }
}

fn render_footer(f: &mut Frame<'_>, area: Rect, app: &App, theme: Theme, now: Instant) {
    let key = |k: &'static str, what: &'static str| {
        [Span::styled(k, theme.strong()), Span::styled(format!(" {what}   "), theme.muted())]
    };
    let causes = app.map().map_or(0, |m| m.causes.len());
    let line = if app.searching {
        let caret = if ((now - app.selected_at).as_millis() / 530).is_multiple_of(2) || !app.motion
        {
            "▏"
        } else {
            " "
        };
        Line::from(vec![
            Span::styled("/", theme.accent()),
            Span::styled(app.search.clone(), theme.strong()),
            Span::styled(caret, theme.accent()),
            Span::styled("   enter keep   esc clear", theme.muted()),
        ])
    } else if let Some(e) = &app.error {
        Line::from(vec![
            Span::styled("Rescan failed: ", theme.fg(ERROR)),
            Span::styled(e.clone(), theme.muted()),
        ])
    } else {
        Line::from(
            [
                key("↑↓", "move"),
                if causes > 1 {
                    [
                        Span::styled(format!("1–{causes}"), theme.strong()),
                        Span::styled(" cause   ", theme.muted()),
                    ]
                } else {
                    key("1", "cause")
                },
                key("/", "search"),
                key("r", "rescan"),
                key("q", "quit"),
            ]
            .concat(),
        )
    };
    f.render_widget(line, area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_fits_its_column() {
        assert_eq!(fit_tail("packages/web/tsconfig.json", 12), "…config.json");
        assert_eq!(fit_head("@acme/design-tokens", 8), "@acme/d…");
        assert_eq!(pad("ui", 4), "ui  ");
        assert_eq!(right("7", 3), "  7");
    }

    #[test]
    fn easing_settles_at_one() {
        assert!(ease(0.0).abs() < f32::EPSILON);
        assert!((ease(1.0) - 1.0).abs() < f32::EPSILON);
        assert!(ease(0.5) > 0.5);
    }
}
