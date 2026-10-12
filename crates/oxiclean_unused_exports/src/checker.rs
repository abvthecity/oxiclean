use std::collections::{HashMap, HashSet};

use oxiclean_core::{
    Analysis, BuildOptions, CheckResult, Export, ExportForm, ExportKind, Finding, FindingKind,
    ModuleGraph, ModuleId, Result, RulePolicy, UnusedExport, Used,
};
use tracing::info;

use crate::config::{Config, DEFAULT_THRESHOLD, UNUSED_EXPORTS_RULE, UNUSED_TYPES_RULE};

/// Tags that keep an export out of the report, as in knip.
const IGNORE_TAGS: [&str; 3] = ["public", "beta", "alias"];

/// Which of the two checks to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unused {
    /// Values, functions, classes and namespaces: knip's `exports`.
    Exports,
    /// Types, interfaces and enums: knip's `types`. See [`Export::is_type`].
    Types,
}

impl Unused {
    /// The rule's name in `.oxicleanrc.json`.
    #[must_use]
    pub fn rule(self) -> &'static str {
        match self {
            Self::Exports => UNUSED_EXPORTS_RULE,
            Self::Types => UNUSED_TYPES_RULE,
        }
    }
}

/// Runs the unused exports check described by `cfg`.
///
/// # Errors
/// Fails if the project root can't be found, the config file is invalid, or
/// no entry files are found.
pub fn run_unused_exports_check(cfg: &Config) -> Result<CheckResult> {
    run(cfg, Unused::Exports)
}

/// Runs the unused types check described by `cfg`.
///
/// # Errors
/// As for [`run_unused_exports_check`].
pub fn run_unused_types_check(cfg: &Config) -> Result<CheckResult> {
    run(cfg, Unused::Types)
}

fn run(cfg: &Config, which: Unused) -> Result<CheckResult> {
    info!("Starting {} check", which.rule());
    // Type-only and lazy imports use exports too, so they are resolved as
    // well, and the files only they reach are analysed. Files no entry
    // reaches are not analysed at all: they are unused files, not files
    // with unused exports.
    let analysis =
        cfg.common.analyze_package_entries(BuildOptions::default().resolve_passive(true))?;
    let policy = analysis.policy(which.rule(), DEFAULT_THRESHOLD)?;
    Ok(check(&analysis, &policy, which))
}

/// Reports each export of the kind `which` asks for that nothing uses. See
/// [`unused_exports`] for what counts as a use. Build the analysis with
/// [`BuildOptions::resolve_passive`], or uses through type-only imports are
/// missed.
#[must_use]
pub fn check(analysis: &Analysis, policy: &RulePolicy, which: Unused) -> CheckResult {
    if policy.min_threshold().is_none() {
        info!("{} is disabled", which.rule());
        return analysis.finish(policy, Vec::new());
    }
    let graph = &analysis.graph;
    let findings: Vec<Finding> = unused_exports(graph, &analysis.entries)
        .into_iter()
        .filter(|(_, e)| e.is_type() == (which == Unused::Types))
        .map(|(id, e)| {
            let unused = UnusedExport::new(e.name.clone(), e.line, e.kind);
            Finding::new(graph.module(id).path.clone(), FindingKind::UnusedExport(unused), 1)
        })
        .collect();
    info!("{} check complete. Found {} unused", which.rule(), findings.len());
    analysis.finish(policy, findings)
}

/// The exports nothing uses, in module order, as knip's `exports` and
/// `types` find them:
///
/// - Exports of entry files are used, and so is everything they re-export.
/// - An import uses the export it names, whether or not the binding is
///   referenced; type-only imports count. A namespace import uses the
///   members accessed on it, or every export when it is used as a whole.
/// - A re-export (`export { a } from './x'`, or an imported `a` exported
///   again) is an export of its own, and using it uses `a` in `./x`.
///   `export * from './x'` passes on the names the module doesn't export
///   itself.
/// - A used function, class or variable uses the exports its signature
///   refers to in type positions ([`Export::signature_refs`]).
/// - Using an export of a JavaScript file uses the same export of its
///   declaration file ([`oxiclean_core::Module::declaration`]).
/// - Exports tagged `@public`, `@beta` or `@alias` are never reported, nor
///   are files in `node_modules`.
#[must_use]
pub fn unused_exports<'g>(
    graph: &'g ModuleGraph,
    entries: &[ModuleId],
) -> Vec<(ModuleId, &'g Export)> {
    let mut usage = Usage::new(graph);
    for &entry in entries {
        usage.queue.push(Task::All(entry));
    }
    for (i, module) in graph.modules().enumerate() {
        let from = usage.id(i);
        for u in &module.uses.usages {
            let Some(target) = usage.target(from, &u.request) else { continue };
            let task = match &u.used {
                Used::Name(name) => Task::Name(target, name),
                Used::Member { name, member } => Task::Member(target, name, member),
                Used::All => Task::All(target),
                Used::Loader
                    if graph.module(target).exports.iter().any(|e| e.name == "default") =>
                {
                    Task::Name(target, "default")
                }
                _ => Task::All(target),
            };
            usage.queue.push(task);
        }
    }
    usage.run();

    let entry_set: HashSet<ModuleId> = entries.iter().copied().collect();
    let mut unused = Vec::new();
    for (i, module) in graph.modules().enumerate() {
        let id = usage.id(i);
        if entry_set.contains(&id) || module.in_node_modules() || usage.all[i] {
            continue;
        }
        let mut reported: HashSet<&str> = HashSet::new();
        for e in &module.exports {
            // `export * as ns from './x'` is not an export of its own in knip.
            let whole_namespace = e.form == ExportForm::ReExport
                && e.forwarded.as_ref().is_some_and(|f| f.name.is_none());
            if usage.names[i].contains(e.name.as_str())
                || usage.partly[i].contains(e.name.as_str())
                || whole_namespace
                || IGNORE_TAGS.iter().any(|t| e.has_tag(t))
                || !reported.insert(&e.name)
            {
                continue;
            }
            unused.push((id, e));
        }
    }
    unused
}

/// Whether using `e` uses the export it passes on: re-exports, and
/// imports exported again unchanged.
fn passes_on(e: &Export) -> bool {
    matches!(e.form, ExportForm::Specifier | ExportForm::Default | ExportForm::ReExport)
}

#[derive(Debug, Clone, Copy)]
enum Task<'g> {
    /// The module's export of that name is used.
    Name(ModuleId, &'g str),
    /// A member of the module's export of that name is used: `x.a` on an
    /// imported `x`.
    Member(ModuleId, &'g str, &'g str),
    /// Every export of the module is used.
    All(ModuleId),
    /// Every export of the module but `default` is used, through
    /// `export * from` in a module whose exports are all used.
    Star(ModuleId),
}

struct Usage<'g> {
    graph: &'g ModuleGraph,
    /// Each module's imports, by request, to the module they load. Built on
    /// first use.
    targets: Vec<Option<HashMap<&'g str, ModuleId>>>,
    /// The module ids, by position in [`ModuleGraph::modules`].
    ids: Vec<ModuleId>,
    all: Vec<bool>,
    star: Vec<bool>,
    names: Vec<HashSet<&'g str>>,
    /// Exports used only through members, by module.
    partly: Vec<HashSet<&'g str>>,
    members: HashSet<(ModuleId, &'g str, &'g str)>,
    queue: Vec<Task<'g>>,
}

impl<'g> Usage<'g> {
    fn new(graph: &'g ModuleGraph) -> Self {
        let n = graph.len();
        let ids = graph.modules().filter_map(|m| graph.id(&m.path)).collect();
        Self {
            graph,
            targets: vec![None; n],
            ids,
            all: vec![false; n],
            star: vec![false; n],
            names: vec![HashSet::new(); n],
            partly: vec![HashSet::new(); n],
            members: HashSet::new(),
            queue: Vec::new(),
        }
    }

    fn id(&self, index: usize) -> ModuleId {
        self.ids[index]
    }

    /// The module `request` loads from `from`.
    fn target(&mut self, from: ModuleId, request: &str) -> Option<ModuleId> {
        let graph = self.graph;
        self.targets[from.index()]
            .get_or_insert_with(|| {
                let module = graph.module(from);
                let mut map = HashMap::new();
                for edge in module.edges.iter().chain(&module.passive_edges) {
                    map.entry(edge.request.as_str()).or_insert(edge.target);
                }
                map
            })
            .get(request)
            .copied()
    }

    fn run(&mut self) {
        while let Some(task) = self.queue.pop() {
            match task {
                Task::Name(m, name) => self.use_name(m, name),
                Task::Member(m, name, member) => self.use_member(m, name, member),
                Task::All(m) => self.use_all(m),
                Task::Star(m) => self.use_star(m),
            }
        }
    }

    fn use_name(&mut self, m: ModuleId, name: &'g str) {
        let i = m.index();
        if self.all[i] || !self.names[i].insert(name) {
            return;
        }
        let module = self.graph.module(m);
        // The declaration file of a JavaScript file exports the same names.
        if let Some(d) = module.declaration {
            self.queue.push(Task::Name(d, name));
        }
        let mut own = false;
        for e in module.exports.iter().filter(|e| e.name == name) {
            own = true;
            self.export_used(m, e);
        }
        // `export *` passes on what the module doesn't export itself.
        if !own && name != "default" {
            for request in &module.uses.reexport_all {
                if let Some(t) = self.target(m, request) {
                    self.queue.push(Task::Name(t, name));
                }
            }
        }
    }

    /// Like [`Self::use_name`], but a namespace the export passes on is used
    /// only for `member`.
    fn use_member(&mut self, m: ModuleId, name: &'g str, member: &'g str) {
        let i = m.index();
        if self.all[i] || self.names[i].contains(name) || !self.members.insert((m, name, member)) {
            return;
        }
        self.partly[i].insert(name);
        let module = self.graph.module(m);
        if let Some(d) = module.declaration {
            self.queue.push(Task::Member(d, name, member));
        }
        let mut own = false;
        for e in module.exports.iter().filter(|e| e.name == name) {
            own = true;
            match &e.forwarded {
                Some(f) if passes_on(e) => {
                    if let Some(t) = self.target(m, &f.from) {
                        self.queue.push(match &f.name {
                            None => Task::Name(t, member),
                            Some(n) => Task::Member(t, n, member),
                        });
                    }
                }
                _ => self.signature_used(m, e),
            }
        }
        if !own && name != "default" {
            for request in &module.uses.reexport_all {
                if let Some(t) = self.target(m, request) {
                    self.queue.push(Task::Member(t, name, member));
                }
            }
        }
    }

    fn use_all(&mut self, m: ModuleId) {
        let i = m.index();
        if std::mem::replace(&mut self.all[i], true) {
            return;
        }
        let module = self.graph.module(m);
        if let Some(d) = module.declaration {
            self.queue.push(Task::All(d));
        }
        for e in &module.exports {
            self.names[i].insert(&e.name);
            self.export_used(m, e);
        }
        self.reexport_all(m);
    }

    fn use_star(&mut self, m: ModuleId) {
        let i = m.index();
        if std::mem::replace(&mut self.star[i], true) {
            return;
        }
        if let Some(d) = self.graph.module(m).declaration {
            self.queue.push(Task::Star(d));
        }
        for e in &self.graph.module(m).exports {
            if e.name != "default" {
                self.queue.push(Task::Name(m, &e.name));
            }
        }
        self.reexport_all(m);
    }

    fn reexport_all(&mut self, m: ModuleId) {
        for request in &self.graph.module(m).uses.reexport_all {
            if let Some(t) = self.target(m, request) {
                self.queue.push(Task::Star(t));
            }
        }
    }

    /// Follows what a used export `e` of `m` uses in turn: the export it
    /// passes on, and the declarations its signature refers to.
    fn export_used(&mut self, m: ModuleId, e: &'g Export) {
        if passes_on(e)
            && let Some(f) = &e.forwarded
            && let Some(t) = self.target(m, &f.from)
        {
            self.queue.push(match &f.name {
                Some(name) => Task::Name(t, name),
                None => Task::All(t),
            });
        }
        self.signature_used(m, e);
    }

    /// Uses the declarations a used export's signature refers to.
    fn signature_used(&mut self, m: ModuleId, e: &'g Export) {
        // knip doesn't count references from types, interfaces and enums.
        if matches!(e.kind, ExportKind::Type | ExportKind::Interface | ExportKind::Enum) {
            return;
        }
        let module = self.graph.module(m);
        for r in &e.signature_refs {
            for x in &module.exports {
                if x.forwarded.is_none() && x.local.as_deref() == Some(r.as_str()) {
                    self.queue.push(Task::Name(m, &x.name));
                }
            }
        }
    }
}
