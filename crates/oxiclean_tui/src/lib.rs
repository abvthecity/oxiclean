//! Interactive terminal views for oxiclean.
//!
//! [`run_map`] opens the program map: which tsconfig programs type-check
//! each package, why, and how much of that work repeats. The scan runs on
//! its own thread while the view draws its progress, so the first frame
//! appears at once.

mod app;
mod theme;
mod ui;

use std::{
    io::IsTerminal,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use oxiclean_core::CommonArgs;
use ratatui::crossterm::event::{self, Event, KeyEventKind};

use crate::{app::App, theme::Theme};

/// How often to redraw while something moves: about 60 frames a second.
const FRAME: Duration = Duration::from_millis(16);

/// How long to wait for a key while nothing moves.
const IDLE: Duration = Duration::from_millis(250);

/// Opens the program map for the workspace `common` names and runs it
/// until the user quits.
///
/// # Errors
/// Fails if stdout isn't a terminal, or the terminal can't be set up or
/// drawn to. A scan that fails is shown in the view, not returned.
pub fn run_map(common: CommonArgs) -> Result<()> {
    if !std::io::stdout().is_terminal() {
        bail!(
            "`oxiclean map` is interactive and needs a terminal; use --format json for the map as data"
        );
    }
    let mut terminal = ratatui::try_init().context("could not set up the terminal")?;
    let theme = Theme::from_env();
    let mut app = App::new(common, Instant::now());
    let result = (|| -> Result<()> {
        while !app.quit {
            let now = Instant::now();
            app.tick(now);
            terminal.draw(|f| ui::render(f, &mut app, theme, now)).context("could not draw")?;
            let wait = if app.animating(Instant::now()) { FRAME } else { IDLE };
            if event::poll(wait).context("could not read input")? {
                // Resizes need no handling: the next frame fills the new size.
                if let Event::Key(key) = event::read().context("could not read input")?
                    && key.kind != KeyEventKind::Release
                {
                    app.on_key(key, Instant::now());
                }
            }
        }
        Ok(())
    })();
    ratatui::try_restore().context("could not restore the terminal")?;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxiclean_typecheck::{
        TsConfigs,
        program_map::{self, Progress},
    };
    use ratatui::{
        Terminal,
        backend::TestBackend,
        crossterm::event::{KeyCode, KeyEvent},
    };
    use std::{fs, path::Path};
    use tempfile::TempDir;

    fn write(root: &Path, path: &str, text: &str) {
        let p = root.join(path);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }

    /// A pnpm workspace of `apps` apps that each import the source of
    /// `shared` through `paths`, a package nothing imports, and a lint
    /// tsconfig over everything.
    fn workspace(apps: usize) -> TempDir {
        let t = TempDir::new().unwrap();
        let root = t.path();
        write(root, "package.json", r#"{ "private": true }"#);
        write(root, "pnpm-workspace.yaml", "packages:\n  - 'apps/*'\n  - 'packages/*'\n");
        write(
            root,
            "tsconfig.base.json",
            r#"{ "compilerOptions": { "baseUrl": ".", "paths": { "@acme/*": ["packages/*/src"] } } }"#,
        );
        write(
            root,
            "tsconfig.eslint.json",
            r#"{ "extends": "./tsconfig.base.json", "include": ["apps", "packages"] }"#,
        );
        for name in ["shared", "quiet"] {
            write(
                root,
                &format!("packages/{name}/package.json"),
                &format!(r#"{{ "name": "@acme/{name}" }}"#),
            );
            write(
                root,
                &format!("packages/{name}/tsconfig.json"),
                r#"{ "extends": "../../tsconfig.base.json", "include": ["src"] }"#,
            );
            write(root, &format!("packages/{name}/src/index.ts"), "export const x = 1;\n");
        }
        for i in 0..apps {
            write(
                root,
                &format!("apps/app{i}/package.json"),
                &format!(r#"{{ "name": "app{i}" }}"#),
            );
            write(
                root,
                &format!("apps/app{i}/tsconfig.json"),
                r#"{ "extends": "../../tsconfig.base.json", "include": ["src"] }"#,
            );
            write(
                root,
                &format!("apps/app{i}/src/main.ts"),
                "import { x } from '@acme/shared';\nexport const y = x;\n",
            );
        }
        t
    }

    fn app(apps: usize) -> (TempDir, App) {
        let t = workspace(apps);
        let common = CommonArgs::with_root(t.path());
        let map = program_map::scan(&common, &Progress::default()).unwrap();
        (t, App::with_map(map, Instant::now()))
    }

    fn screen(app: &mut App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        let theme = Theme { color: false };
        terminal.draw(|f| ui::render(f, app, theme, Instant::now())).unwrap();
        let buffer = terminal.backend().buffer();
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn press(app: &mut App, code: KeyCode) {
        app.on_key(KeyEvent::from(code), Instant::now());
    }

    #[test]
    fn draws_summary_causes_list_and_detail() {
        let (_t, mut app) = app(3);
        let s = screen(&mut app, 140, 40);
        assert!(s.contains("oxiclean map"), "{s}");
        assert!(s.contains("of type checking repeats work"), "{s}");
        assert!(s.contains("source"), "{s}");
        assert!(s.contains("lint tsconfigs that include everything"), "{s}");
        // shared is checked by its owner, three apps and the lint config.
        assert!(s.contains("› @acme/shared"), "{s}");
        assert!(s.contains("5×"), "{s}");
        assert!(s.contains("apps/app0/src/main.ts:1"), "{s}");
        assert!(s.contains("→ add a project reference"), "{s}");
    }

    /// `cargo test -p oxiclean_tui preview -- --ignored --nocapture` prints
    /// the view at a few sizes, to look at while changing the layout.
    #[test]
    #[ignore = "prints screens to look at"]
    #[allow(clippy::print_stderr)]
    fn preview() {
        let (_t, mut app) = app(12);
        // OXICLEAN_PREVIEW_ROOT previews a real workspace instead.
        if let Some(root) = std::env::var_os("OXICLEAN_PREVIEW_ROOT") {
            let map =
                program_map::scan(&CommonArgs::with_root(root), &Progress::default()).unwrap();
            app = App::with_map(map, Instant::now());
        }
        for (w, h) in [(150, 42), (96, 36)] {
            eprintln!("{}\n{}", screen(&mut app, w, h), "=".repeat(usize::from(w)));
        }
        press(&mut app, KeyCode::Char('2'));
        eprintln!("{}", screen(&mut app, 150, 42));
        let t = workspace(2);
        let mut scanning = App::new(CommonArgs::with_root(t.path()), Instant::now());
        eprintln!("{}", screen(&mut scanning, 100, 24));
    }

    #[test]
    fn narrow_terminals_stack_the_detail_under_the_list() {
        let (_t, mut app) = app(2);
        let s = screen(&mut app, 80, 40);
        assert!(s.contains("› @acme/shared"), "{s}");
        assert!(s.contains("SOURCE"), "{s}");
    }

    #[test]
    fn keys_move_filter_and_search() {
        let (_t, mut app) = app(3);
        let names = |app: &App| -> Vec<String> {
            let map = app.map().unwrap();
            app.visible.iter().map(|&i| map.packages[i].name.clone()).collect()
        };
        assert_eq!(names(&app)[0], "@acme/shared");
        press(&mut app, KeyCode::Down);
        assert_eq!(app.selected, 1);
        press(&mut app, KeyCode::Char('G'));
        assert_eq!(app.selected, app.visible.len() - 1);
        press(&mut app, KeyCode::Down);
        assert_eq!(app.selected, app.visible.len() - 1);

        // The first cause is source imports: only shared has them.
        press(&mut app, KeyCode::Char('1'));
        assert_eq!(names(&app), ["@acme/shared"]);
        press(&mut app, KeyCode::Char('1'));
        assert!(app.cause.is_none());

        press(&mut app, KeyCode::Char('/'));
        for c in "quiet".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        assert_eq!(names(&app), ["@acme/quiet"]);
        press(&mut app, KeyCode::Esc);
        assert!(app.search.is_empty() && !app.searching);
        press(&mut app, KeyCode::Char('q'));
        assert!(app.quit);
    }

    #[test]
    fn long_lists_scroll_with_the_selection() {
        let (_t, mut app) = app(30);
        press(&mut app, KeyCode::Char('G'));
        let s = screen(&mut app, 140, 30);
        let last = app.package().unwrap().name.clone();
        assert!(s.contains(&format!("› {last}")), "{s}");
        assert!(app.offset > 0);
    }

    #[test]
    fn the_scan_shows_its_progress_then_the_map() {
        let t = workspace(2);
        let mut app = App::new(CommonArgs::with_root(t.path()), Instant::now());
        let s = screen(&mut app, 100, 30);
        assert!(s.contains("Following imports") && s.contains("Reading workspace"), "{s}");
        let deadline = Instant::now() + Duration::from_secs(20);
        while app.loaded.is_none() && Instant::now() < deadline {
            app.tick(Instant::now());
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(app.loaded.is_some(), "{:?}", app.error);
        let s = screen(&mut app, 100, 30);
        assert!(s.contains("› @acme/shared"), "{s}");
    }

    #[test]
    fn a_failed_scan_says_why() {
        let mut app = App::new(CommonArgs::with_root("/no/such/oxiclean/root"), Instant::now());
        let deadline = Instant::now() + Duration::from_secs(10);
        while app.error.is_none() && Instant::now() < deadline {
            app.tick(Instant::now());
            std::thread::sleep(Duration::from_millis(5));
        }
        let s = screen(&mut app, 100, 30);
        assert!(s.contains("The scan failed"), "{s}");
        // Keep the unused-import lint quiet for TsConfigs on this path.
        let _ = TsConfigs::default();
    }
}
