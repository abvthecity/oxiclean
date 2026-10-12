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

use crate::constants::JS_TS_EXTENSIONS;

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

/// Options that control which edges the builder follows.
#[derive(Debug, Clone, Copy, Default)]
#[non_exhaustive]
pub struct BuildOptions {
    /// Follow dynamic `import()` edges. They are skipped by default because a
    /// lazy import is loaded as a separate chunk, so it is the remedy for
    /// bloat rather than a cause of it.
    pub follow_dynamic: bool,
}

impl BuildOptions {
    #[must_use]
    pub fn follow_dynamic(mut self, yes: bool) -> Self {
        self.follow_dynamic = yes;
        self
    }
}

/// An import graph with cycles collapsed into SCCs.
#[derive(Debug)]
pub struct ModuleGraph {
    modules: Vec<Module>,
    index: HashMap<PathBuf, ModuleId>,
    failures: Vec<ParseFailure>,
    /// SCC id of each module.
    comp: Vec<u32>,
    /// Number of modules in each SCC.
    comp_size: Vec<u32>,
    /// Deduplicated edges between distinct SCCs.
    comp_succ: Vec<Vec<u32>>,
    /// SCC ids in reverse topological order (every SCC appears after all SCCs
    /// it can reach). This is the order Tarjan's algorithm emits them in.
    comp_order: Vec<u32>,
}

/// Returns true for files the parser understands. Anything else that an import
/// resolves to (CSS, JSON, images, ...) is kept as a leaf module.
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
                    if !is_parseable(path) {
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
            comp_order: Vec::new(),
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

    /// Longest import chain below each module, counted in edges, indexed by
    /// [`ModuleId::index`].
    ///
    /// Modules in the same import cycle form one level and share a depth, so
    /// the result is well defined and independent of traversal order.
    #[must_use]
    pub fn depths(&self) -> Vec<usize> {
        let mut comp_depth = vec![0usize; self.comp_size.len()];
        // Reverse topological order: successors are final before predecessors.
        for &c in &self.comp_order {
            let c = c as usize;
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

        let topo: Vec<u32> = self.comp_order.iter().rev().copied().collect();
        let counts: HashMap<u32, usize> = comp_queries
            .par_chunks(64)
            .flat_map_iter(|chunk| {
                let mut mask = vec![0u64; self.comp_size.len()];
                for (bit, &c) in chunk.iter().enumerate() {
                    mask[c as usize] |= 1 << bit;
                }
                let mut totals = [0usize; 64];
                for &c in &topo {
                    let m = mask[c as usize];
                    if m == 0 {
                        continue;
                    }
                    for &s in &self.comp_succ[c as usize] {
                        mask[s as usize] |= m;
                    }
                    let size = self.comp_size[c as usize] as usize;
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
        const UNVISITED: u32 = u32::MAX;
        let n = self.modules.len();
        let mut index_of = vec![UNVISITED; n];
        let mut lowlink = vec![0u32; n];
        let mut on_stack = vec![false; n];
        let mut stack: Vec<u32> = Vec::new();
        let mut comp = vec![UNVISITED; n];
        let mut comp_size: Vec<u32> = Vec::new();
        let mut comp_order: Vec<u32> = Vec::new();
        let mut next_index = 0u32;
        // (node, next edge to look at)
        let mut call: Vec<(u32, usize)> = Vec::new();

        for root in 0..n {
            if index_of[root] != UNVISITED {
                continue;
            }
            call.push((root as u32, 0));
            while let Some(&mut (v, ref mut edge_pos)) = call.last_mut() {
                let vi = v as usize;
                if *edge_pos == 0 && index_of[vi] == UNVISITED {
                    index_of[vi] = next_index;
                    lowlink[vi] = next_index;
                    next_index += 1;
                    stack.push(v);
                    on_stack[vi] = true;
                }
                let edges = &self.modules[vi].edges;
                if let Some(edge) = edges.get(*edge_pos) {
                    *edge_pos += 1;
                    let w = edge.target.idx();
                    if index_of[w] == UNVISITED {
                        call.push((w as u32, 0));
                    } else if on_stack[w] {
                        lowlink[vi] = lowlink[vi].min(index_of[w]);
                    }
                    continue;
                }
                // All edges of v done.
                call.pop();
                if lowlink[vi] == index_of[vi] {
                    let c = comp_size.len() as u32;
                    let mut size = 0;
                    loop {
                        let w = stack.pop().expect("tarjan stack holds v");
                        on_stack[w as usize] = false;
                        comp[w as usize] = c;
                        size += 1;
                        if w == v {
                            break;
                        }
                    }
                    comp_size.push(size);
                    comp_order.push(c);
                }
                if let Some(&(parent, _)) = call.last() {
                    let p = parent as usize;
                    lowlink[p] = lowlink[p].min(lowlink[vi]);
                }
            }
        }

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
        self.comp_order = comp_order;
    }
}

fn intern(
    modules: &mut Vec<Module>,
    index: &mut HashMap<PathBuf, ModuleId>,
    path: PathBuf,
) -> ModuleId {
    let id = ModuleId(u32::try_from(modules.len()).expect("more than u32::MAX modules"));
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
