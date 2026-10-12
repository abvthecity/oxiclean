//! The module graph shared by every checker.
//!
//! The graph is built once per run: files are parsed and their imports resolved
//! in parallel, one breadth-first level at a time, and every module is interned
//! to a dense [`ModuleId`]. Analyses then run on plain integer adjacency lists.
//!
//! Import cycles are collapsed into strongly connected components (SCCs) before
//! any analysis, so results never depend on traversal order or thread
//! scheduling, and every traversal is iterative so deep chains cannot overflow
//! the stack.

use std::{
    collections::HashMap,
    fmt::Display,
    path::{Path, PathBuf},
};

use rayon::prelude::*;
use tracing::{debug, trace, warn};

use crate::{
    constants::JS_TS_EXTENSIONS,
    sequencer::{Components, sequence, strongly_connected},
};

/// Dense index of a module in a [`ModuleGraph`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ModuleId(u32);

impl ModuleId {
    /// Position of the module in per-module vectors such as
    /// [`ModuleGraph::depths`].
    #[inline]
    #[must_use]
    pub fn index(self) -> usize {
        self.0 as usize
    }

    #[inline]
    fn idx(self) -> usize {
        self.index()
    }
}

/// One import as seen by the graph builder, before resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawImport {
    /// The specifier as written in source, e.g. `./utils` or `react`.
    pub request: String,
    /// Whether this is a lazy `import()` rather than an eager import.
    pub dynamic: bool,
    /// 1-based line of the import, or 0 when unknown.
    pub line: u32,
}

/// What the builder learns from parsing one file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedFile {
    pub imports: Vec<RawImport>,
    /// Set when the file had syntax errors. Its imports are still used, but
    /// may be incomplete, so the file is reported as a [`ParseFailure`].
    pub error: Option<String>,
}

/// A resolved import edge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edge {
    pub target: ModuleId,
    pub request: String,
    /// 1-based line of the import in the importing file, or 0 when unknown.
    pub line: u32,
}

/// A module (file) in the graph.
#[derive(Debug, Clone)]
pub struct Module {
    pub path: PathBuf,
    pub edges: Vec<Edge>,
}

/// A file that could not be read or parsed cleanly. The module stays in the
/// graph so it is still counted, but its imports may be missing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseFailure {
    pub path: PathBuf,
    pub message: String,
}

/// A circular import: each module imports the next, and the last imports the
/// first.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Cycle {
    /// The modules in import order, starting with the one whose path sorts
    /// first.
    pub modules: Vec<ModuleId>,
    /// Number of modules that import each other, directly or indirectly,
    /// together with this cycle's: the size of its strongly connected
    /// component. A large group means one cycle is part of a wider tangle.
    pub group_size: usize,
}

/// Options that control which edges the builder follows.
#[derive(Debug, Clone, Copy, Default)]
#[non_exhaustive]
pub struct BuildOptions {
    /// Follow dynamic `import()` edges. They are skipped by default because a
    /// lazy import is loaded as a separate chunk, so it is the remedy for
    /// bloat rather than a cause of it.
    pub follow_dynamic: bool,
    /// Parse files inside `node_modules` and follow their imports. Off by
    /// default: an installed package is then one leaf module, which keeps
    /// runs fast and findings about the project's own code.
    pub follow_node_modules: bool,
}

impl BuildOptions {
    #[must_use]
    pub fn follow_dynamic(mut self, yes: bool) -> Self {
        self.follow_dynamic = yes;
        self
    }

    #[must_use]
    pub fn follow_node_modules(mut self, yes: bool) -> Self {
        self.follow_node_modules = yes;
        self
    }
}

/// An import graph with cycles collapsed into SCCs.
#[derive(Debug)]
pub struct ModuleGraph {
    modules: Vec<Module>,
    index: HashMap<PathBuf, ModuleId>,
    failures: Vec<ParseFailure>,
    /// SCC id of each module. Ids are in reverse topological order, as
    /// Tarjan's algorithm emits them: an SCC's id is higher than the id of
    /// every SCC it can reach.
    comp: Vec<u32>,
    /// Number of modules in each SCC.
    comp_size: Vec<u32>,
    /// Deduplicated edges between distinct SCCs.
    comp_succ: Vec<Vec<u32>>,
}

/// Returns true for files the parser understands. Anything else that an import
/// resolves to (CSS, JSON, images, ...) is kept as a leaf module.
fn in_node_modules(path: &Path) -> bool {
    path.components().any(|c| c.as_os_str() == "node_modules")
}

fn is_parseable(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|ext| JS_TS_EXTENSIONS.contains(&ext))
}

impl ModuleGraph {
    /// Builds the graph reachable from `entries`.
    ///
    /// `parse` lists the imports of a JS/TS file. `resolve` maps an import
    /// request made from a file to the file it loads, or `None` when it can't
    /// be resolved (such edges are dropped). Both run in parallel.
    pub fn build<P, R, E>(entries: &[PathBuf], parse: P, resolve: R, opts: BuildOptions) -> Self
    where
        P: Fn(&Path) -> Result<ParsedFile, E> + Sync,
        R: Fn(&Path, &str) -> Option<PathBuf> + Sync,
        E: Display,
    {
        let mut modules: Vec<Module> = Vec::with_capacity(entries.len());
        let mut index: HashMap<PathBuf, ModuleId> = HashMap::with_capacity(entries.len());
        let mut failures = Vec::new();

        let mut frontier: Vec<ModuleId> = Vec::new();
        for entry in entries {
            if !index.contains_key(entry) {
                let id = intern(&mut modules, &mut index, entry.clone());
                frontier.push(id);
            }
        }

        while !frontier.is_empty() {
            trace!("Graph frontier: {} modules", frontier.len());
            // Parse and resolve the whole frontier in parallel. Only this step
            // touches the file system; interning below is sequential and cheap.
            type Expanded = (ModuleId, Vec<(String, u32, PathBuf)>, Option<String>);
            let expanded: Vec<Expanded> = frontier
                .par_iter()
                .map(|&id| {
                    let path = &modules[id.idx()].path;
                    if !is_parseable(path) || (!opts.follow_node_modules && in_node_modules(path)) {
                        return (id, Vec::new(), None);
                    }
                    let parsed = match parse(path) {
                        Ok(parsed) => parsed,
                        Err(e) => return (id, Vec::new(), Some(e.to_string())),
                    };
                    let resolved = parsed
                        .imports
                        .into_iter()
                        .filter(|imp| opts.follow_dynamic || !imp.dynamic)
                        .filter_map(|imp| {
                            let target = resolve(path, &imp.request);
                            if target.is_none() {
                                trace!("Unresolved '{}' from {}", imp.request, path.display());
                            }
                            target.map(|t| (imp.request, imp.line, t))
                        })
                        .collect();
                    (id, resolved, parsed.error)
                })
                .collect();

            let mut next = Vec::new();
            for (id, resolved, error) in expanded {
                let mut edges = Vec::with_capacity(resolved.len());
                for (request, line, target_path) in resolved {
                    let target = match index.get(&target_path) {
                        Some(&t) => t,
                        None => {
                            let t = intern(&mut modules, &mut index, target_path);
                            next.push(t);
                            t
                        }
                    };
                    edges.push(Edge { target, request, line });
                }
                modules[id.idx()].edges = edges;
                if let Some(message) = error {
                    let path = modules[id.idx()].path.clone();
                    warn!("Could not parse {} cleanly: {message}", path.display());
                    failures.push(ParseFailure { path, message });
                }
            }
            frontier = next;
        }

        failures.sort_by(|a, b| a.path.cmp(&b.path));
        debug!("Built module graph: {} modules, {} parse failures", modules.len(), failures.len());

        let mut graph = Self {
            modules,
            index,
            failures,
            comp: Vec::new(),
            comp_size: Vec::new(),
            comp_succ: Vec::new(),
        };
        graph.condense();
        graph
    }

    /// Number of modules in the graph.
    #[must_use]
    pub fn len(&self) -> usize {
        self.modules.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.modules.is_empty()
    }

    #[must_use]
    pub fn id(&self, path: &Path) -> Option<ModuleId> {
        self.index.get(path).copied()
    }

    #[must_use]
    pub fn module(&self, id: ModuleId) -> &Module {
        &self.modules[id.idx()]
    }

    /// Files that could not be read or parsed, sorted by path.
    #[must_use]
    pub fn failures(&self) -> &[ParseFailure] {
        &self.failures
    }

    /// Whether two modules are part of the same import cycle.
    #[must_use]
    pub fn same_cycle(&self, a: ModuleId, b: ModuleId) -> bool {
        self.comp[a.idx()] == self.comp[b.idx()]
    }

    /// Import cycles between files outside `node_modules`.
    ///
    /// Cycles are found by [`sequence`], the port of pnpm's graph sequencer,
    /// which picks short cycles: the longest of the shortest cycles through
    /// each file it searches from. The import that closes each reported cycle
    /// (from its last file back to its first) is then set aside and the
    /// search repeats until no cycle is left. So every circular import runs
    /// through the closing import of at least one reported cycle, and
    /// removing those imports would leave the graph without cycles.
    ///
    /// Files are taken in path order, so the result does not depend on how
    /// the graph was built.
    #[must_use]
    pub fn cycles(&self) -> Vec<Cycle> {
        let n = self.modules.len();
        // Imports still considered, as targets per module: runtime imports
        // between files outside node_modules, one per target.
        let outside: Vec<bool> = self.modules.iter().map(|m| !in_node_modules(&m.path)).collect();
        let mut deps: Vec<Vec<usize>> = self
            .modules
            .iter()
            .enumerate()
            .map(|(i, m)| {
                let mut targets: Vec<usize> = Vec::new();
                if outside[i] {
                    for t in m.edges.iter().map(|e| e.target.idx()) {
                        if outside[t] && !targets.contains(&t) {
                            targets.push(t);
                        }
                    }
                }
                targets
            })
            .collect();

        let mut by_path: Vec<usize> = (0..n).filter(|&i| outside[i]).collect();
        by_path.sort_by(|&a, &b| self.modules[a].path.cmp(&self.modules[b].path));

        let mut cycles = Vec::new();
        loop {
            let components = strongly_connected(n, |v| deps[v].iter().copied(), |v| !outside[v]);
            let included: Vec<usize> = by_path
                .iter()
                .copied()
                .filter(|&i| {
                    components.sizes[components.of[i] as usize] > 1 || deps[i].contains(&i)
                })
                .collect();
            if included.is_empty() {
                break;
            }
            let found = sequence(&deps, &included).cycles;
            debug_assert!(!found.is_empty(), "every component with a cycle yields one");
            if found.is_empty() {
                break;
            }
            for cycle in found {
                let last = cycle[cycle.len() - 1];
                deps[last].retain(|&t| t != cycle[0]);
                let mut modules: Vec<ModuleId> =
                    cycle.into_iter().map(|i| ModuleId(len_u32(i))).collect();
                // Start at the file whose path sorts first. The sequencer
                // already does so; rotating keeps that a guarantee.
                if let Some(first) = (0..modules.len()).min_by(|&a, &b| {
                    self.module(modules[a]).path.cmp(&self.module(modules[b]).path)
                }) {
                    modules.rotate_left(first);
                }
                let group_size = self.comp_size[self.comp[modules[0].idx()] as usize] as usize;
                cycles.push(Cycle { modules, group_size });
            }
        }
        cycles.sort_by(|a, b| {
            let paths =
                |c: &Cycle| c.modules.iter().map(|&m| &self.module(m).path).collect::<Vec<_>>();
            paths(a).cmp(&paths(b))
        });
        cycles
    }

    /// The first import in `from` that loads `to`, in source order.
    #[must_use]
    pub fn edge(&self, from: ModuleId, to: ModuleId) -> Option<&Edge> {
        self.module(from).edges.iter().find(|e| e.target == to)
    }

    /// Longest import chain below each module, counted in edges, indexed by
    /// [`ModuleId::index`].
    ///
    /// Modules in the same import cycle form one level and share a depth, so
    /// the result is well defined and independent of traversal order.
    #[must_use]
    pub fn depths(&self) -> Vec<usize> {
        let mut comp_depth = vec![0usize; self.comp_size.len()];
        // Reverse topological order: successors are final before predecessors.
        for c in 0..self.comp_size.len() {
            comp_depth[c] =
                self.comp_succ[c].iter().map(|&s| comp_depth[s as usize] + 1).max().unwrap_or(0);
        }
        self.comp.iter().map(|&c| comp_depth[c as usize]).collect()
    }

    /// For each queried module, the number of unique modules reachable from it,
    /// including itself.
    ///
    /// Queries are processed 64 at a time: each SCC carries a `u64` with one bit
    /// per query, and bits are pushed along edges in topological order. That is
    /// one linear pass over the graph per 64 queries, run in parallel.
    #[must_use]
    pub fn reachable_counts(&self, queries: &[ModuleId]) -> Vec<usize> {
        // Deduplicate by SCC: every module in a cycle has the same answer.
        let mut comp_queries: Vec<u32> = queries.iter().map(|q| self.comp[q.idx()]).collect();
        comp_queries.sort_unstable();
        comp_queries.dedup();

        let counts: HashMap<u32, usize> = comp_queries
            .par_chunks(64)
            .flat_map_iter(|chunk| {
                let mut mask = vec![0u64; self.comp_size.len()];
                for (bit, &c) in chunk.iter().enumerate() {
                    mask[c as usize] |= 1 << bit;
                }
                let mut totals = [0usize; 64];
                // Topological order: an SCC is final before its successors.
                for c in (0..self.comp_size.len()).rev() {
                    let m = mask[c];
                    if m == 0 {
                        continue;
                    }
                    for &s in &self.comp_succ[c] {
                        mask[s as usize] |= m;
                    }
                    let size = self.comp_size[c] as usize;
                    let mut bits = m;
                    while bits != 0 {
                        totals[bits.trailing_zeros() as usize] += size;
                        bits &= bits - 1;
                    }
                }
                chunk.iter().enumerate().map(move |(bit, &c)| (c, totals[bit])).collect::<Vec<_>>()
            })
            .collect();

        queries.iter().map(|q| counts[&self.comp[q.idx()]]).collect()
    }

    /// Computes SCCs with an iterative Tarjan's algorithm and the condensed DAG.
    fn condense(&mut self) {
        let n = self.modules.len();
        let modules = &self.modules;
        let Components { of: comp, sizes: comp_size } =
            strongly_connected(n, |v| modules[v].edges.iter().map(|e| e.target.idx()), |_| false);

        let mut comp_succ: Vec<Vec<u32>> = vec![Vec::new(); comp_size.len()];
        for (v, module) in self.modules.iter().enumerate() {
            let cv = comp[v];
            for edge in &module.edges {
                let cw = comp[edge.target.idx()];
                if cw != cv {
                    comp_succ[cv as usize].push(cw);
                }
            }
        }
        for succ in &mut comp_succ {
            succ.sort_unstable();
            succ.dedup();
        }

        debug!("Condensed {} modules into {} components", n, comp_size.len());
        self.comp = comp;
        self.comp_size = comp_size;
        self.comp_succ = comp_succ;
    }
}

fn len_u32(n: usize) -> u32 {
    u32::try_from(n).expect("more than u32::MAX modules")
}

fn intern(
    modules: &mut Vec<Module>,
    index: &mut HashMap<PathBuf, ModuleId>,
    path: PathBuf,
) -> ModuleId {
    let id = ModuleId(len_u32(modules.len()));
    index.insert(path.clone(), id);
    modules.push(Module { path, edges: Vec::new() });
    id
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::convert::Infallible;

    /// Builds a graph from an in-memory adjacency list. Files are `<name>.js`.
    fn graph(edges: &[(&str, &[&str])], entries: &[&str]) -> ModuleGraph {
        let adj: HashMap<PathBuf, Vec<&str>> = edges
            .iter()
            .map(|(from, to)| (PathBuf::from(format!("{from}.js")), to.to_vec()))
            .collect();
        let entries: Vec<PathBuf> =
            entries.iter().map(|e| PathBuf::from(format!("{e}.js"))).collect();
        ModuleGraph::build(
            &entries,
            |p: &Path| -> Result<ParsedFile, Infallible> {
                let imports = adj
                    .get(p)
                    .map(|to| {
                        to.iter()
                            .map(|t| RawImport {
                                request: (*t).to_string(),
                                dynamic: false,
                                line: 0,
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                Ok(ParsedFile { imports, error: None })
            },
            |_, req| Some(PathBuf::from(format!("{req}.js"))),
            BuildOptions::default(),
        )
    }

    fn depth_of(g: &ModuleGraph, name: &str) -> usize {
        g.depths()[g.id(Path::new(&format!("{name}.js"))).unwrap().idx()]
    }

    fn count_of(g: &ModuleGraph, name: &str) -> usize {
        g.reachable_counts(&[g.id(Path::new(&format!("{name}.js"))).unwrap()])[0]
    }

    fn cycle_names(g: &ModuleGraph) -> Vec<(Vec<String>, usize)> {
        g.cycles()
            .into_iter()
            .map(|c| {
                let names = c
                    .modules
                    .iter()
                    .map(|&m| {
                        g.module(m).path.to_string_lossy().trim_end_matches(".js").to_string()
                    })
                    .collect();
                (names, c.group_size)
            })
            .collect()
    }

    #[test]
    fn cycles_start_at_the_first_path_and_follow_imports() {
        // c -> a -> b -> c, plus a self-import and an acyclic tail.
        let edges: &[(&str, &[&str])] = &[
            ("e", &["c", "s"]),
            ("c", &["a"]),
            ("a", &["b"]),
            ("b", &["c"]),
            ("s", &["s", "t"]),
            ("t", &[]),
        ];
        let expected =
            vec![(vec!["a".to_string(), "b".into(), "c".into()], 3), (vec!["s".into()], 1)];
        assert_eq!(cycle_names(&graph(edges, &["e"])), expected);
        // The order modules were discovered in doesn't matter.
        assert_eq!(cycle_names(&graph(edges, &["b", "s", "e"])), expected);
    }

    #[test]
    fn every_cycle_in_a_tangle_is_reported() {
        // a <-> b and b <-> c share b: one tangle of three files, two cycles.
        let edges: &[(&str, &[&str])] = &[("a", &["b"]), ("b", &["a", "c"]), ("c", &["b"])];
        assert_eq!(
            cycle_names(&graph(edges, &["a"])),
            vec![(vec!["a".to_string(), "b".into()], 3), (vec!["b".to_string(), "c".into()], 3)]
        );
    }

    /// On random graphs, each reported cycle follows real imports, and
    /// removing the import that closes each one leaves no cycle.
    #[test]
    fn closing_imports_break_every_cycle() {
        let mut seed = 0x2545_f491_u64;
        let mut rand = |n: u64| {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (seed >> 33) % n
        };
        for _ in 0..300 {
            let n = 1 + rand(14) as usize;
            let density = 1 + rand(4);
            let names: Vec<String> = (0..n).map(|i| format!("m{i}")).collect();
            let targets: Vec<Vec<&str>> = (0..n)
                .map(|_| (0..n).filter(|_| rand(10) < density).map(|j| names[j].as_str()).collect())
                .collect();
            let edges: Vec<(&str, &[&str])> =
                names.iter().zip(&targets).map(|(n, t)| (n.as_str(), t.as_slice())).collect();
            let g = graph(&edges, &names.iter().map(String::as_str).collect::<Vec<_>>());

            let mut adj: Vec<Vec<usize>> = (0..n)
                .map(|i| {
                    g.module(ModuleId(len_u32(i))).edges.iter().map(|e| e.target.idx()).collect()
                })
                .collect();
            for cycle in g.cycles() {
                let m = &cycle.modules;
                for (i, from) in m.iter().enumerate() {
                    let to = m[(i + 1) % m.len()];
                    assert!(g.edge(*from, to).is_some(), "{cycle:?} follows imports");
                }
                let (last, first) = (m[m.len() - 1].idx(), m[0].idx());
                adj[last].retain(|&t| t != first);
            }
            let left = strongly_connected(n, |v| adj[v].iter().copied(), |_| false);
            for (v, targets) in adj.iter().enumerate() {
                let alone = left.sizes[left.of[v] as usize] == 1;
                assert!(alone && !targets.contains(&v), "{edges:?}");
            }
        }
    }

    #[test]
    fn no_cycles_in_a_dag() {
        let edges: &[(&str, &[&str])] = &[("a", &["b", "c"]), ("b", &["c"]), ("c", &[])];
        assert!(graph(edges, &["a"]).cycles().is_empty());
    }

    #[test]
    fn cycles_inside_node_modules_are_left_out() {
        let edges: &[(&str, &[&str])] = &[
            ("a", &["node_modules/x/i"]),
            ("node_modules/x/i", &["node_modules/x/j"]),
            ("node_modules/x/j", &["node_modules/x/i"]),
        ];
        let g = ModuleGraph::build(
            &[PathBuf::from("a.js")],
            |p: &Path| -> Result<ParsedFile, Infallible> {
                let name = p.to_string_lossy();
                let name = name.trim_end_matches(".js");
                let imports = edges
                    .iter()
                    .find(|(from, _)| *from == name)
                    .map(|(_, to)| {
                        to.iter()
                            .map(|t| RawImport {
                                request: (*t).to_string(),
                                dynamic: false,
                                line: 1,
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                Ok(ParsedFile { imports, error: None })
            },
            |_, req| Some(PathBuf::from(format!("{req}.js"))),
            BuildOptions::default().follow_node_modules(true),
        );
        assert_eq!(g.len(), 3);
        assert!(g.cycles().is_empty());
    }

    #[test]
    fn depth_of_chain() {
        let g = graph(&[("a", &["b"]), ("b", &["c"]), ("c", &[])], &["a"]);
        assert_eq!(depth_of(&g, "a"), 2);
        assert_eq!(depth_of(&g, "b"), 1);
        assert_eq!(depth_of(&g, "c"), 0);
    }

    #[test]
    fn depth_with_cycle_is_order_independent() {
        // a <-> b, and a -> x -> y -> z. The cycle is one level.
        let edges: &[(&str, &[&str])] =
            &[("a", &["b", "x"]), ("b", &["a"]), ("x", &["y"]), ("y", &["z"]), ("z", &[])];
        let g1 = graph(edges, &["a", "b"]);
        let g2 = graph(edges, &["b", "a"]);
        for g in [&g1, &g2] {
            assert_eq!(depth_of(g, "a"), 3);
            assert_eq!(depth_of(g, "b"), 3);
            assert_eq!(depth_of(g, "x"), 2);
        }
    }

    #[test]
    fn reachable_counts_include_self_and_cycles() {
        let edges: &[(&str, &[&str])] =
            &[("e", &["a", "b"]), ("a", &["c"]), ("b", &["c"]), ("c", &["a"]), ("d", &[])];
        let g = graph(edges, &["e", "d"]);
        assert_eq!(count_of(&g, "e"), 4);
        assert_eq!(count_of(&g, "a"), 2); // a, c
        assert_eq!(count_of(&g, "c"), 2);
        assert_eq!(count_of(&g, "b"), 3); // b, c, a
        assert_eq!(count_of(&g, "d"), 1);
    }

    #[test]
    fn reachable_counts_more_than_64_queries() {
        // 200-module chain: m0 -> m1 -> ... -> m199. Every module is queried.
        let names: Vec<String> = (0..200).map(|i| format!("m{i}")).collect();
        let targets: Vec<Vec<&str>> =
            (0..200).map(|i| if i < 199 { vec![names[i + 1].as_str()] } else { vec![] }).collect();
        let edges: Vec<(&str, &[&str])> =
            names.iter().zip(&targets).map(|(n, t)| (n.as_str(), t.as_slice())).collect();
        let g = graph(&edges, &["m0"]);
        let ids: Vec<ModuleId> =
            names.iter().map(|n| g.id(Path::new(&format!("{n}.js"))).unwrap()).collect();
        let counts = g.reachable_counts(&ids);
        for (i, c) in counts.iter().enumerate() {
            assert_eq!(*c, 200 - i);
        }
    }

    #[test]
    fn deep_chain_does_not_overflow() {
        let n = 200_000;
        let names: Vec<String> = (0..n).map(|i| format!("m{i}")).collect();
        let targets: Vec<Vec<&str>> =
            (0..n).map(|i| if i + 1 < n { vec![names[i + 1].as_str()] } else { vec![] }).collect();
        let edges: Vec<(&str, &[&str])> =
            names.iter().zip(&targets).map(|(n, t)| (n.as_str(), t.as_slice())).collect();
        let g = graph(&edges, &["m0"]);
        assert_eq!(depth_of(&g, "m0"), n - 1);
        assert_eq!(count_of(&g, "m0"), n);
    }

    #[test]
    fn dynamic_edges_skipped_unless_requested() {
        let parse = |p: &Path| -> Result<ParsedFile, Infallible> {
            let imports = if p == Path::new("a.js") {
                vec![RawImport { request: "b".into(), dynamic: true, line: 0 }]
            } else {
                vec![]
            };
            Ok(ParsedFile { imports, error: None })
        };
        let resolve = |_: &Path, req: &str| Some(PathBuf::from(format!("{req}.js")));
        let entries = [PathBuf::from("a.js")];
        let g = ModuleGraph::build(&entries, parse, resolve, BuildOptions::default());
        assert_eq!(g.len(), 1);
        let g = ModuleGraph::build(
            &entries,
            parse,
            resolve,
            BuildOptions::default().follow_dynamic(true),
        );
        assert_eq!(g.len(), 2);
    }

    #[test]
    fn parse_failures_are_recorded_and_module_kept() {
        let entries = [PathBuf::from("a.js"), PathBuf::from("b.js"), PathBuf::from("style.css")];
        let g = ModuleGraph::build(
            &entries,
            |p: &Path| match p.to_str() {
                Some("a.js") => Err("unreadable"),
                Some("b.js") => Ok(ParsedFile {
                    imports: vec![RawImport { request: "c".into(), dynamic: false, line: 0 }],
                    error: Some("syntax error".into()),
                }),
                _ => Ok(ParsedFile::default()),
            },
            |_, req| Some(PathBuf::from(format!("{req}.js"))),
            BuildOptions::default(),
        );
        // a, b, style.css, plus c, recovered from b despite its syntax error.
        assert_eq!(g.len(), 4);
        let failed: Vec<&Path> = g.failures().iter().map(|f| f.path.as_path()).collect();
        assert_eq!(failed, vec![Path::new("a.js"), Path::new("b.js")]);
    }
}
