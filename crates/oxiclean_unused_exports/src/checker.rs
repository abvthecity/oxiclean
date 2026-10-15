use std::collections::{HashMap, HashSet};

use oxiclean_core::{
    Analysis, BuildOptions, CheckResult, CommandResult, Export, ExportForm, ExportKind,
    ExportMember, Finding, FindingKind, Module, ModuleGraph, ModuleId, Result, RulePolicy,
    UnusedExport, Used, report::ReportStyle,
};
use tracing::info;

use crate::config::{
    Config, DEFAULT_THRESHOLD, UNUSED_ENUM_MEMBERS_RULE, UNUSED_ENUM_MEMBERS_STYLE,
    UNUSED_EXPORTS_RULE, UNUSED_EXPORTS_STYLE, UNUSED_NAMESPACE_MEMBERS_RULE,
    UNUSED_NAMESPACE_MEMBERS_STYLE, UNUSED_TYPES_RULE, UNUSED_TYPES_STYLE,
};

/// Tags that keep an export out of the report, as in knip.
const IGNORE_TAGS: [&str; 3] = ["public", "beta", "alias"];

/// Which of the two checks to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unused {
    /// Values, functions, classes and namespaces: knip's `exports`.
    Exports,
    /// Types, interfaces and enums: knip's `types`. See [`Export::is_type`].
    Types,
    /// Keys of used enums: knip's `enumMembers`.
    EnumMembers,
    /// What used namespaces export: knip's `namespaceMembers`.
    NamespaceMembers,
}

impl Unused {
    /// Every rule, in the order the `unused` command runs them.
    pub const ALL: [Self; 4] =
        [Self::Exports, Self::Types, Self::EnumMembers, Self::NamespaceMembers];

    /// The rule's name in `.oxicleanrc.json`.
    #[must_use]
    pub fn rule(self) -> &'static str {
        match self {
            Self::Exports => UNUSED_EXPORTS_RULE,
            Self::Types => UNUSED_TYPES_RULE,
            Self::EnumMembers => UNUSED_ENUM_MEMBERS_RULE,
            Self::NamespaceMembers => UNUSED_NAMESPACE_MEMBERS_RULE,
        }
    }

    /// How the rule's findings are worded in the terminal report.
    #[must_use]
    pub fn style(self) -> ReportStyle {
        match self {
            Self::Exports => UNUSED_EXPORTS_STYLE,
            Self::Types => UNUSED_TYPES_STYLE,
            Self::EnumMembers => UNUSED_ENUM_MEMBERS_STYLE,
            Self::NamespaceMembers => UNUSED_NAMESPACE_MEMBERS_STYLE,
        }
    }

    /// Whether the rule reports members, which the graph must read.
    fn members(self) -> bool {
        matches!(self, Self::EnumMembers | Self::NamespaceMembers)
    }
}

/// Runs each of the rules in [`Unused::ALL`] the config file leaves on,
/// from one module graph and one pass over its uses. Rules that are off
/// don't run.
///
/// # Errors
/// As for [`run_unused_exports_check`].
pub fn run_unused_command(cfg: &Config) -> Result<CommandResult> {
    let workspace = cfg.common.workspace()?;
    let mut out = CommandResult::new(workspace.root.clone());
    let on: Vec<Unused> = Unused::ALL.into_iter().filter(|w| workspace.enabled(w.rule())).collect();
    if on.is_empty() {
        info!("Every unused rule is off");
        return Ok(out);
    }
    let analysis =
        workspace.analyze_package_entries(build_options(on.iter().any(|w| w.members())))?;
    let usage = ExportUsage::new(&analysis.graph, &analysis.entries);
    for which in on {
        let policy = analysis.policy(which.rule(), DEFAULT_THRESHOLD)?;
        out.push(which.rule(), which.style(), check_usage(&analysis, &usage, &policy, which));
    }
    Ok(out)
}

/// What the module graph of the checks of exports reads. Type-only and
/// lazy imports use exports too, so they are resolved as well, and the
/// files only they reach are analysed. Files no entry reaches are not
/// analysed at all: they are unused files, not files with unused exports.
fn build_options(members: bool) -> BuildOptions {
    BuildOptions::default().resolve_passive(true).read_exports(true).read_members(members)
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

/// Runs the unused enum members check described by `cfg`.
///
/// # Errors
/// As for [`run_unused_exports_check`].
pub fn run_unused_enum_members_check(cfg: &Config) -> Result<CheckResult> {
    run(cfg, Unused::EnumMembers)
}

/// Runs the unused namespace members check described by `cfg`.
///
/// # Errors
/// As for [`run_unused_exports_check`].
pub fn run_unused_namespace_members_check(cfg: &Config) -> Result<CheckResult> {
    run(cfg, Unused::NamespaceMembers)
}

fn run(cfg: &Config, which: Unused) -> Result<CheckResult> {
    info!("Starting {} check", which.rule());
    let analysis = cfg.common.analyze_package_entries(build_options(which.members()))?;
    let policy = analysis.policy(which.rule(), DEFAULT_THRESHOLD)?;
    Ok(check(&analysis, &policy, which))
}

/// Reports each export of the kind `which` asks for that nothing uses. See
/// [`unused_exports`] for what counts as a use. Build the analysis with
/// [`BuildOptions::resolve_passive`], or uses through type-only imports are
/// missed, and with [`BuildOptions::read_exports`], or nothing is used.
#[must_use]
pub fn check(analysis: &Analysis, policy: &RulePolicy, which: Unused) -> CheckResult {
    if policy.min_threshold().is_none() {
        info!("{} is disabled", which.rule());
        return analysis.finish(policy, Vec::new());
    }
    let usage = ExportUsage::new(&analysis.graph, &analysis.entries);
    check_usage(analysis, &usage, policy, which)
}

/// As [`check`], with the uses of `analysis`'s graph already followed, so
/// several rules can share them.
#[must_use]
pub fn check_usage(
    analysis: &Analysis,
    usage: &ExportUsage<'_>,
    policy: &RulePolicy,
    which: Unused,
) -> CheckResult {
    if policy.min_threshold().is_none() {
        info!("{} is disabled", which.rule());
        return analysis.finish(policy, Vec::new());
    }
    let graph = &analysis.graph;
    let findings: Vec<Finding> = match which {
        Unused::Exports | Unused::Types => usage
            .unused_exports()
            .into_iter()
            .filter(|(_, e)| e.is_type() == (which == Unused::Types))
            .map(|(id, e)| {
                let unused = UnusedExport::new(e.name.clone(), e.line, e.kind);
                Finding::new(graph.module(id).path.clone(), FindingKind::UnusedExport(unused), 1)
            })
            .collect(),
        Unused::EnumMembers | Unused::NamespaceMembers => usage
            .unused_members()
            .into_iter()
            .filter(|(_, e, _)| (e.kind == ExportKind::Enum) == (which == Unused::EnumMembers))
            .map(|(id, e, m)| {
                let name = format!("{}.{}", e.name, m.name());
                let unused = UnusedExport::new(name, m.line, m.kind);
                Finding::new(graph.module(id).path.clone(), FindingKind::UnusedMember(unused), 1)
            })
            .collect(),
    };
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
    ExportUsage::new(graph, entries).unused_exports()
}

/// The members of used enums and namespaces nothing uses, in module order,
/// as knip's `enumMembers` and `namespaceMembers` find them, with the
/// export each belongs to:
///
/// - Only exports something uses are checked: an unused export is reported
///   as such, not its members. Neither are exports of entry files, or of
///   modules used as a whole, nor exports tagged `@public`, `@beta` or
///   `@alias`.
/// - `E.a`, `E['a']` and `N.a.b`, in values and types, use that member and
///   the members it is in. A member used in its own file is used.
/// - Every member is used when the export is read as a whole: `E[key]`,
///   `Object.keys(E)`, `const e = E`, the type `E`, `typeof E`, or a
///   re-export from an entry file. Passing `E` around uses none.
/// - Members tagged `@public`, `@beta` or `@alias` are never reported.
#[must_use]
pub fn unused_members<'g>(
    graph: &'g ModuleGraph,
    entries: &[ModuleId],
) -> Vec<(ModuleId, &'g Export, &'g ExportMember)> {
    ExportUsage::new(graph, entries).unused_members()
}

/// Which exports and members of a graph are used, from its entries.
#[derive(Debug)]
pub struct ExportUsage<'g> {
    tracker: Tracker<'g>,
    entries: HashSet<ModuleId>,
}

impl<'g> ExportUsage<'g> {
    /// Follows every use in `graph`, starting from `entries`.
    #[must_use]
    pub fn new(graph: &'g ModuleGraph, entries: &[ModuleId]) -> Self {
        let mut usage = Tracker::new(graph);
        for &entry in entries {
            usage.queue.push(Task::All(entry));
        }
        for (from, module) in graph.iter() {
            for u in &module.uses.usages {
                let Some(target) = usage.target(from, &u.request) else { continue };
                let task = match &u.used {
                    Used::Name(name) => Task::Name(target, name),
                    Used::Member { name, member } => Task::Member(target, name, member),
                    Used::Whole(name) => Task::Whole(target, name),
                    Used::Loader if usage.index(target).by_name.contains_key("default") => {
                        Task::Name(target, "default")
                    }
                    // A loader of a module without a default export,
                    // `Used::All`, and anything `Used` may add later use
                    // every export.
                    _ => Task::All(target),
                };
                usage.queue.push(task);
            }
        }
        usage.run();
        Self { tracker: usage, entries: entries.iter().copied().collect() }
    }

    /// Modules whose exports may be reported: not entries, not in
    /// `node_modules`, and not used as a whole.
    fn checked(&self) -> impl Iterator<Item = (ModuleId, &'g Module)> + '_ {
        self.tracker.graph.iter().filter(|&(id, module)| {
            !self.entries.contains(&id)
                && !module.in_node_modules()
                && !self.tracker.all[id.index()]
        })
    }

    /// See [`unused_exports`].
    #[must_use]
    pub fn unused_exports(&self) -> Vec<(ModuleId, &'g Export)> {
        let usage = &self.tracker;
        let mut unused = Vec::new();
        for (id, module) in self.checked() {
            // A value and a type may share a name; each is reported once.
            let mut reported: HashSet<(&str, bool)> = HashSet::new();
            for e in &module.exports {
                // `export * as ns from './x'` is not an export of its own in knip.
                let whole_namespace = e.form == ExportForm::ReExport
                    && e.forwarded.as_ref().is_some_and(|f| f.name.is_none());
                if usage.is_used(id, &e.name)
                    || whole_namespace
                    || IGNORE_TAGS.iter().any(|t| e.has_tag(t))
                    || !reported.insert((&e.name, e.is_type()))
                {
                    continue;
                }
                unused.push((id, e));
            }
        }
        unused
    }

    /// See [`unused_members`].
    #[must_use]
    pub fn unused_members(&self) -> Vec<(ModuleId, &'g Export, &'g ExportMember)> {
        let usage = &self.tracker;
        // The member paths used on each export, and every path that contains
        // one: reading `a.b` reads `a` too.
        let mut paths: HashSet<(ModuleId, &str, &[String])> = HashSet::new();
        for &(m, name, member) in &usage.members {
            for end in 1..=member.len() {
                paths.insert((m, name, &member[..end]));
            }
        }
        let mut unused = Vec::new();
        for (id, module) in self.checked() {
            for e in &module.exports {
                if e.members.is_empty()
                    || !usage.is_used(id, &e.name)
                    || usage.whole.contains(&(id, e.name.as_str()))
                    || IGNORE_TAGS.iter().any(|t| e.has_tag(t))
                {
                    continue;
                }
                for m in &e.members {
                    if m.used_in_file
                        || IGNORE_TAGS.iter().any(|t| m.has_tag(t))
                        || paths.contains(&(id, e.name.as_str(), m.path.as_slice()))
                    {
                        continue;
                    }
                    unused.push((id, e, m));
                }
            }
        }
        unused
    }
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
    Member(ModuleId, &'g str, &'g [String]),
    /// The module's export of that name and every one of its members are
    /// used.
    Whole(ModuleId, &'g str),
    /// Every export of the module is used.
    All(ModuleId),
    /// Every export of the module but `default` is used, through
    /// `export * from` in a module whose exports are all used.
    Star(ModuleId),
}

/// What the worklist has found used so far.
#[derive(Debug)]
struct Tracker<'g> {
    graph: &'g ModuleGraph,
    /// Each module's imports, by request, to the module they load. Built on
    /// first use.
    targets: Vec<Option<HashMap<&'g str, ModuleId>>>,
    /// Each module's exports by name and by local binding. Built on first use.
    indexes: Vec<Option<ExportIndex<'g>>>,
    all: Vec<bool>,
    star: Vec<bool>,
    names: Vec<HashSet<&'g str>>,
    /// Exports used only through members, by module.
    partly: Vec<HashSet<&'g str>>,
    /// Member paths used on each export.
    members: HashSet<(ModuleId, &'g str, &'g [String])>,
    /// Exports whose every member is used.
    whole: HashSet<(ModuleId, &'g str)>,
    queue: Vec<Task<'g>>,
}

/// A module's exports, indexed so that generated files with thousands of
/// exports don't make each lookup a scan.
#[derive(Debug, Default)]
struct ExportIndex<'g> {
    by_name: HashMap<&'g str, Vec<&'g Export>>,
    /// Names of the module's exports by the local binding they export,
    /// imported bindings exported again included.
    by_local: HashMap<&'g str, Vec<&'g str>>,
}

impl<'g> Tracker<'g> {
    fn new(graph: &'g ModuleGraph) -> Self {
        let n = graph.len();
        Self {
            graph,
            targets: vec![None; n],
            indexes: std::iter::repeat_with(|| None).take(n).collect(),
            all: vec![false; n],
            star: vec![false; n],
            names: vec![HashSet::new(); n],
            partly: vec![HashSet::new(); n],
            members: HashSet::new(),
            whole: HashSet::new(),
            queue: Vec::new(),
        }
    }

    fn index(&mut self, m: ModuleId) -> &ExportIndex<'g> {
        let graph = self.graph;
        self.indexes[m.index()].get_or_insert_with(|| {
            let mut index = ExportIndex::default();
            for e in &graph.module(m).exports {
                index.by_name.entry(e.name.as_str()).or_default().push(e);
                if let Some(local) = &e.local {
                    index.by_local.entry(local.as_str()).or_default().push(&e.name);
                }
            }
            index
        })
    }

    /// The exports of `m` called `name`.
    fn named(&mut self, m: ModuleId, name: &str) -> Vec<&'g Export> {
        self.index(m).by_name.get(name).cloned().unwrap_or_default()
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
                Task::Whole(m, name) => self.use_whole(m, name),
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
        let own = self.named(m, name);
        for &e in &own {
            self.export_used(m, e, false);
        }
        let own = !own.is_empty();
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
    fn use_member(&mut self, m: ModuleId, name: &'g str, member: &'g [String]) {
        let i = m.index();
        if self.all[i] || !self.members.insert((m, name, member)) {
            return;
        }
        // An export already used by name still passes the path on, to the
        // members of what it re-exports.
        let by_name = self.names[i].contains(name);
        if !by_name {
            self.partly[i].insert(name);
        }
        let module = self.graph.module(m);
        if let Some(d) = module.declaration {
            self.queue.push(Task::Member(d, name, member));
        }
        let own = self.named(m, name);
        for &e in &own {
            match &e.forwarded {
                Some(f) if passes_on(e) => {
                    if let Some(t) = self.target(m, &f.from) {
                        let task = match (&f.name, member.split_first()) {
                            // `x.a.b` on a namespace `x` of `./t` reads
                            // member `b` of `a` there.
                            (None, Some((first, []))) => Task::Name(t, first),
                            (None, Some((first, rest))) => Task::Member(t, first, rest),
                            (None, None) => continue,
                            (Some(n), _) => Task::Member(t, n, member),
                        };
                        self.queue.push(task);
                    }
                }
                _ if !by_name => self.signature_used(m, e),
                _ => {}
            }
        }
        if own.is_empty() && name != "default" {
            for request in &module.uses.reexport_all {
                if let Some(t) = self.target(m, request) {
                    self.queue.push(Task::Member(t, name, member));
                }
            }
        }
    }

    /// Like [`Self::use_name`], and every member of the export is used,
    /// along with what it passes on.
    fn use_whole(&mut self, m: ModuleId, name: &'g str) {
        if self.all[m.index()] || !self.whole.insert((m, name)) {
            return;
        }
        self.queue.push(Task::Name(m, name));
        let module = self.graph.module(m);
        if let Some(d) = module.declaration {
            self.queue.push(Task::Whole(d, name));
        }
        let own = self.named(m, name);
        for &e in &own {
            if passes_on(e)
                && let Some(f) = &e.forwarded
                && let Some(t) = self.target(m, &f.from)
            {
                self.queue.push(match &f.name {
                    Some(n) => Task::Whole(t, n),
                    None => Task::All(t),
                });
            }
        }
        if own.is_empty() && name != "default" {
            for request in &module.uses.reexport_all {
                if let Some(t) = self.target(m, request) {
                    self.queue.push(Task::Whole(t, name));
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
            self.export_used(m, e, true);
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
        // What a module used as a whole passes on is used as a whole.
        for e in &self.graph.module(m).exports {
            if e.name != "default" {
                self.queue.push(Task::Whole(m, &e.name));
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
    /// passes on, as a whole when `whole`, and the declarations its
    /// signature refers to.
    fn export_used(&mut self, m: ModuleId, e: &'g Export, whole: bool) {
        if passes_on(e)
            && let Some(f) = &e.forwarded
            && let Some(t) = self.target(m, &f.from)
        {
            self.queue.push(match &f.name {
                Some(name) if whole => Task::Whole(t, name),
                Some(name) => Task::Name(t, name),
                None => Task::All(t),
            });
        }
        self.signature_used(m, e);
    }

    /// Whether the export `name` of `m` is used, by name or through members.
    fn is_used(&self, m: ModuleId, name: &str) -> bool {
        let i = m.index();
        self.all[i] || self.names[i].contains(name) || self.partly[i].contains(name)
    }

    /// Uses the declarations a used export's signature refers to.
    fn signature_used(&mut self, m: ModuleId, e: &'g Export) {
        // knip doesn't count references from types, interfaces and enums.
        if matches!(e.kind, ExportKind::Type | ExportKind::Interface | ExportKind::Enum) {
            return;
        }
        for r in &e.signature_refs {
            let names = self.index(m).by_local.get(r.as_str()).cloned().unwrap_or_default();
            self.queue.extend(names.into_iter().map(|name| Task::Name(m, name)));
        }
    }
}
