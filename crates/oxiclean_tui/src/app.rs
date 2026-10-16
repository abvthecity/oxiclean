//! The program map's state: the scan, what is selected and filtered, and
//! the clock its animations run on. Drawing is in [`crate::ui`].

use std::{
    any::Any,
    panic::{self, AssertUnwindSafe},
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

/// The text of a panic, when it has one.
fn panic_message(payload: &(dyn Any + Send)) -> &str {
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("no message")
}

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
                // A panic in the scan becomes an error the view shows; the
                // view's panic hook leaves the terminal alone for it.
                let scanned =
                    panic::catch_unwind(AssertUnwindSafe(|| program_map::scan(&common, &p)));
                let found = match scanned {
                    Ok(found) => found.map_err(|e| oxiclean_core::command::describe(&e)),
                    Err(payload) => Err(format!("the scan crashed: {}", panic_message(&*payload))),
                };
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
        let scan = Scan::start(&common, now);
        let motion = std::env::var_os("OXICLEAN_NO_MOTION").is_none();
        Self::blank(common, Some(scan), motion, now)
    }

    /// A view of a map that is already built, for tests.
    #[cfg(test)]
    pub(crate) fn with_map(map: ProgramMap, now: Instant) -> Self {
        let mut app = Self::blank(CommonArgs::default(), None, false, now);
        app.finish(Ok(map), now);
        app
    }

    fn blank(common: CommonArgs, scan: Option<Scan>, motion: bool, now: Instant) -> Self {
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
            motion,
            quit: false,
        }
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
                // Keep the selected package across a rescan. The old
                // indexes don't fit the new map, so they go first.
                let keep = self.package().map(|p| p.name.clone());
                self.visible.clear();
                self.loaded = Some(Loaded { map, at: now, took });
                self.error = None;
                self.refilter();
                if let Some(name) = keep
                    && let Some(i) = self
                        .visible
                        .iter()
                        .position(|&i| self.map().is_some_and(|m| m.packages[i].name == name))
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

    /// The selected package.
    pub(crate) fn package(&self) -> Option<&PackageChecks> {
        let i = *self.visible.get(self.selected)?;
        self.map()?.packages.get(i)
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

    /// Applies the filters, keeping the selected package selected when it
    /// still shows.
    fn refilter(&mut self) {
        let keep = self.package().map(|p| p.name.clone());
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
        if let (Some(cause), Some(loaded)) = (self.cause, &self.loaded) {
            // Rank by the cause's own repeats.
            let packages = &loaded.map.packages;
            self.visible.sort_by_key(|&i| std::cmp::Reverse(packages[i].repeats_of(cause)));
        }
        let kept = keep.and_then(|name| {
            let packages = &self.loaded.as_ref()?.map.packages;
            self.visible.iter().position(|&i| packages[i].name == name)
        });
        self.selected = kept.unwrap_or(self.selected).min(self.visible.len().saturating_sub(1));
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
                KeyCode::Char(c)
                    if !key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    self.search.push(c);
                }
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
                let n = c.to_digit(10).and_then(|d| usize::try_from(d).ok()).unwrap_or(0);
                let picked =
                    self.map().and_then(|m| m.causes.get(n.checked_sub(1)?)).map(|c| c.cause);
                if let Some(picked) = picked {
                    self.cause = if self.cause == Some(picked) { None } else { Some(picked) };
                    self.refilter();
                    // A new filter starts from its biggest package.
                    self.selected = 0;
                    self.offset = 0;
                    self.selected_at = now;
                }
            }
            KeyCode::Char('r') if self.scan.is_none() => {
                self.scan = Some(Scan::start(&self.common, now));
            }
            _ => {}
        }
    }
}
