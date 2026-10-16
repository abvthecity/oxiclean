//! The program map's state: the scan, what is selected and filtered, and
//! the clock its animations run on. Drawing is in [`crate::ui`].

use std::{
    sync::{
        Arc,
        mpsc::{self, Receiver, TryRecvError},
    },
    thread,
    time::{Duration, Instant},
};

use oxiclean_core::CommonArgs;
use oxiclean_typecheck::program_map::{self, Cause, PackageChecks, Phase, ProgramMap, Progress};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// The stack of the scan's thread: oxc's parser recurses once per level
/// of nesting, as for the command's own thread.
const SCAN_STACK: usize = 64 * 1024 * 1024;

/// How long the numbers and bars take to grow in after a scan.
pub(crate) const REVEAL: Duration = Duration::from_millis(520);

/// How long the detail's bars take to grow in for a new selection.
pub(crate) const SELECT: Duration = Duration::from_millis(220);

/// A scan in progress.
#[derive(Debug)]
pub(crate) struct Scan {
    pub progress: Arc<Progress>,
    pub started: Instant,
    /// When each phase started, as the progress was last seen.
    pub phases: Vec<(Phase, Instant)>,
    rx: Receiver<Result<ProgramMap, String>>,
}

impl Scan {
    /// Starts scanning the workspace `common` names on its own thread.
    fn start(common: &CommonArgs, now: Instant) -> Self {
        let progress = Arc::new(Progress::default());
        let (tx, rx) = mpsc::channel();
        let (p, common) = (Arc::clone(&progress), common.clone());
        let spawned = thread::Builder::new()
            .name("oxiclean-scan".into())
            .stack_size(SCAN_STACK)
            .spawn(move || {
                let found = program_map::scan(&common, &p)
                    .map_err(|e| oxiclean_core::command::describe(&e));
                // The view may have quit already; then nobody wants it.
                let _ = tx.send(found);
            });
        let rx = match spawned {
            Ok(_) => rx,
            Err(e) => {
                let (tx, rx) = mpsc::channel();
                let _ = tx.send(Err(format!("could not start the scan: {e}")));
                rx
            }
        };
        Self { progress, started: now, phases: vec![(Phase::Workspace, now)], rx }
    }

    /// The scan's result, once it has one.
    fn poll(&mut self, now: Instant) -> Option<Result<ProgramMap, String>> {
        let phase = self.progress.phase();
        if self.phases.last().is_none_or(|(p, _)| *p != phase) {
            self.phases.push((phase, now));
        }
        match self.rx.try_recv() {
            Ok(found) => Some(found),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err("the scan stopped unexpectedly".into())),
        }
    }
}

/// A finished scan.
#[derive(Debug)]
pub(crate) struct Loaded {
    pub map: ProgramMap,
    pub at: Instant,
    pub took: Duration,
}

/// Everything the view shows.
#[derive(Debug)]
pub(crate) struct App {
    common: CommonArgs,
    pub scan: Option<Scan>,
    pub loaded: Option<Loaded>,
    pub error: Option<String>,
    /// Show only packages with repeats of this cause.
    pub cause: Option<Cause>,
    pub search: String,
    pub searching: bool,
    /// Indexes into the map's packages that the filters let through.
    pub visible: Vec<usize>,
    /// Position in `visible`.
    pub selected: usize,
    /// First row of `visible` on screen, kept by the view.
    pub offset: usize,
    pub selected_at: Instant,
    /// Whether to animate; off with `OXICLEAN_NO_MOTION`.
    pub motion: bool,
    pub quit: bool,
}

impl App {
    /// A view of the workspace `common` names, scanning it from `now`.
    pub(crate) fn new(common: CommonArgs, now: Instant) -> Self {
        let scan = Some(Scan::start(&common, now));
        Self {
            common,
            scan,
            loaded: None,
            error: None,
            cause: None,
            search: String::new(),
            searching: false,
            visible: Vec::new(),
            selected: 0,
            offset: 0,
            selected_at: now,
            motion: std::env::var_os("OXICLEAN_NO_MOTION").is_none(),
            quit: false,
        }
    }

    /// A view of a map that is already built, for tests.
    #[cfg(test)]
    pub(crate) fn with_map(map: ProgramMap, now: Instant) -> Self {
        let mut app = Self {
            common: CommonArgs::default(),
            scan: None,
            loaded: None,
            error: None,
            cause: None,
            search: String::new(),
            searching: false,
            visible: Vec::new(),
            selected: 0,
            offset: 0,
            selected_at: now,
            motion: false,
            quit: false,
        };
        app.finish(Ok(map), now);
        app
    }

    /// Takes in the scan's result, if it has finished.
    pub(crate) fn tick(&mut self, now: Instant) {
        if let Some(found) = self.scan.as_mut().and_then(|s| s.poll(now)) {
            self.finish(found, now);
        }
    }

    fn finish(&mut self, found: Result<ProgramMap, String>, now: Instant) {
        let took = self.scan.take().map_or(Duration::ZERO, |s| now - s.started);
        match found {
            Ok(map) => {
                // Keep the selected package across a rescan.
                let keep = self.package().map(|p| p.name.clone());
                self.loaded = Some(Loaded { map, at: now, took });
                self.error = None;
                self.refilter();
                if let Some(name) = keep
                    && let Some(i) =
                        self.visible.iter().position(|&i| self.map_package(i).name == name)
                {
                    self.selected = i;
                }
                self.selected_at = now;
            }
            Err(e) => self.error = Some(e),
        }
    }

    /// Whether anything on screen is still moving, so the view redraws at
    /// frame rate rather than waiting for a key.
    pub(crate) fn animating(&self, now: Instant) -> bool {
        let revealing = self.loaded.as_ref().is_some_and(|l| now - l.at < REVEAL);
        self.scan.is_some() || (self.motion && (revealing || now - self.selected_at < SELECT))
    }

    pub(crate) fn map(&self) -> Option<&ProgramMap> {
        self.loaded.as_ref().map(|l| &l.map)
    }

    fn map_package(&self, i: usize) -> &PackageChecks {
        &self.loaded.as_ref().expect("visible packages come from a loaded map").map.packages[i]
    }

    /// The selected package.
    pub(crate) fn package(&self) -> Option<&PackageChecks> {
        let i = *self.visible.get(self.selected)?;
        self.map().map(|m| &m.packages[i])
    }

    /// How much of a package counts under the current filter.
    pub(crate) fn weight(&self, p: &PackageChecks) -> usize {
        self.cause.map_or(p.repeats, |c| p.repeats_of(c))
    }

    /// Packages with repeats the filters leave out, as (repeating,
    /// checked once) counts.
    pub(crate) fn hidden(&self) -> (usize, usize) {
        let Some(map) = self.map() else { return (0, 0) };
        let once = map.packages.iter().filter(|p| p.repeats == 0).count();
        (map.packages.len() - once - self.visible.len(), once)
    }

    fn refilter(&mut self) {
        let needle = self.search.to_lowercase();
        self.visible = match self.map() {
            Some(map) => map
                .packages
                .iter()
                .enumerate()
                .filter(|(_, p)| self.weight(p) > 0)
                .filter(|(_, p)| needle.is_empty() || p.name.to_lowercase().contains(&needle))
                .map(|(i, _)| i)
                .collect(),
            None => Vec::new(),
        };
        if self.cause.is_some() {
            // Rank by the cause's own repeats.
            let map = &self.loaded.as_ref().expect("filtered a loaded map").map;
            let cause = self.cause;
            self.visible.sort_by_key(|&i| {
                std::cmp::Reverse(cause.map_or(0, |c| map.packages[i].repeats_of(c)))
            });
        }
        self.selected = self.selected.min(self.visible.len().saturating_sub(1));
    }

    fn select(&mut self, to: usize, now: Instant) {
        let to = to.min(self.visible.len().saturating_sub(1));
        if to != self.selected {
            self.selected = to;
            self.selected_at = now;
        }
    }

    /// Handles a key press.
    pub(crate) fn on_key(&mut self, key: KeyEvent, now: Instant) {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        if self.searching {
            match key.code {
                KeyCode::Esc => {
                    self.search.clear();
                    self.searching = false;
                }
                KeyCode::Enter => self.searching = false,
                KeyCode::Backspace => {
                    self.search.pop();
                }
                KeyCode::Char(c) => self.search.push(c),
                KeyCode::Down => self.select(self.selected + 1, now),
                KeyCode::Up => self.select(self.selected.saturating_sub(1), now),
                _ => return,
            }
            self.refilter();
            self.selected_at = now;
            return;
        }
        let page = 10;
        match key.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Esc if self.cause.is_none() && self.search.is_empty() => self.quit = true,
            KeyCode::Esc => {
                self.cause = None;
                self.search.clear();
                self.refilter();
            }
            KeyCode::Down | KeyCode::Char('j') => self.select(self.selected + 1, now),
            KeyCode::Up | KeyCode::Char('k') => self.select(self.selected.saturating_sub(1), now),
            KeyCode::PageDown => self.select(self.selected + page, now),
            KeyCode::PageUp => self.select(self.selected.saturating_sub(page), now),
            KeyCode::Home | KeyCode::Char('g') => self.select(0, now),
            KeyCode::End | KeyCode::Char('G') => self.select(usize::MAX, now),
            KeyCode::Char('/') => self.searching = true,
            KeyCode::Char('0') => {
                self.cause = None;
                self.refilter();
            }
            KeyCode::Char(c @ '1'..='9') => {
                let causes: Vec<Cause> = self
                    .map()
                    .map(|m| m.causes.iter().map(|c| c.cause).collect())
                    .unwrap_or_default();
                let n = c.to_digit(10).map_or(0, |d| d as usize);
                if let Some(&picked) = causes.get(n - 1) {
                    self.cause = if self.cause == Some(picked) { None } else { Some(picked) };
                    self.selected = 0;
                    self.offset = 0;
                    self.selected_at = now;
                    self.refilter();
                }
            }
            KeyCode::Char('r') if self.scan.is_none() => {
                self.scan = Some(Scan::start(&self.common, now));
            }
            _ => {}
        }
    }
}
