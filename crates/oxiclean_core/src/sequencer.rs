//! Orders a dependency graph and reports the cycles that prevent a strict
//! order.
//!
//! This is a port of pnpm's [`graph-sequencer`], which pnpm uses to order
//! workspace packages for running scripts and installing. Given each
//! node's dependencies, it produces one deterministic order in which every
//! node comes after its dependencies wherever the graph allows it. When every
//! remaining node waits on another, the graph has a cycle. The sequencer then
//! finds one cycle per strongly connected component, breaks it by removing all
//! of its nodes, and carries on. The cycles it reports are therefore disjoint
//! and between them break every circular dependency.
//!
//! The cycle through a node is the longest of the shortest paths from the node
//! back to itself, so that one report covers as much of a tangle as possible.
//!
//! [`graph-sequencer`]: https://github.com/pnpm/pnpm/tree/main/pnpm11/deps/graph-sequencer

/// The result of [`sequence`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Sequence {
    /// Every included node, dependencies first wherever there is no cycle.
    pub order: Vec<usize>,
    /// The cycles that were broken, each starting at its earliest node in
    /// the `included` order and listed in dependency order: each node depends
    /// on the next, and the last depends on the first.
    pub cycles: Vec<Vec<usize>>,
}

/// Orders the nodes in `included` so that each comes after the nodes it
/// depends on, breaking cycles as described in the [module docs](self).
///
/// `dependencies[n]` lists the nodes `n` depends on, by index. Nodes that are
/// not in `included`, and edges to them, are ignored. Ties are broken by the
/// position in `included`, so the result depends only on the inputs.
///
/// # Panics
/// Panics if a dependency or an included node is not a valid index into
/// `dependencies`, or if `included` has more than `u32::MAX` nodes.
#[must_use]
pub fn sequence<D: AsRef<[usize]>>(dependencies: &[D], included: &[usize]) -> Sequence {
    let graph = Graph::new(dependencies, included);
    let n = graph.deps.len();

    let mut removed = vec![false; n];
    let mut out_degree: Vec<u32> = graph.deps.iter().map(|d| len_u32(d.len())).collect();
    let mut order: Vec<u32> = Vec::with_capacity(n);
    let mut cycles: Vec<Vec<u32>> = Vec::new();
    let mut search = CycleSearch::new(n);

    let mut current: Vec<u32> = (0..n).filter(|&id| out_degree[id] == 0).map(len_u32).collect();
    let mut remaining = n;
    while remaining > 0 {
        let mut next = Vec::new();
        let mut remove = |id: u32, removed: &mut [bool]| {
            removed[id as usize] = true;
            for &parent in &graph.dependents[id as usize] {
                let degree = &mut out_degree[parent as usize];
                if *degree > 0 {
                    *degree -= 1;
                    if *degree == 0 && !removed[parent as usize] {
                        next.push(parent);
                    }
                }
            }
        };

        if current.is_empty() {
            // Every remaining node waits on another, so there are cycles. A
            // cycle through a node stays inside the node's strongly connected
            // component, so only members of a component with two or more
            // nodes, or with a self-import, are searched, and each search
            // stays inside its component.
            let components = strongly_connected(
                n,
                |id| graph.deps[id].iter().map(|&d| d as usize),
                |id| removed[id],
            );
            let broken_before = order.len();
            for id in 0..n {
                if removed[id] || !components.may_hold_cycle(id, &graph.deps[id]) {
                    continue;
                }
                let cycle = search.find(len_u32(id), &graph.deps, &removed, &components.of);
                if cycle.is_empty() {
                    continue;
                }
                for &node in &cycle {
                    remove(node, &mut removed);
                }
                order.extend_from_slice(&cycle);
                cycles.push(cycle);
            }
            let broken = order.len() - broken_before;
            if broken == 0 {
                // Unreachable: a remaining node with dependencies left means
                // a cycle among the remaining nodes. Stop rather than spin.
                debug_assert!(false, "graph sequencer found no cycle to break");
                break;
            }
            remaining -= broken;
        } else {
            for &id in &current {
                remove(id, &mut removed);
            }
            remaining -= current.len();
            order.extend_from_slice(&current);
        }
        // Breaking a cycle removes its nodes one at a time, so a later node of
        // the same cycle can drop to no dependencies just before it is
        // removed itself.
        next.retain(|&id| !removed[id as usize]);
        next.sort_unstable();
        next.dedup();
        current = next;
    }

    Sequence {
        order: order.into_iter().map(|id| included[id as usize]).collect(),
        cycles: cycles
            .into_iter()
            .map(|cycle| cycle.into_iter().map(|id| included[id as usize]).collect())
            .collect(),
    }
}

fn len_u32(n: usize) -> u32 {
    u32::try_from(n).expect("graph sequencer supports at most u32::MAX nodes")
}

/// The included nodes renumbered by their position in `included`, with
/// duplicate edges and edges to other nodes dropped.
struct Graph {
    deps: Vec<Vec<u32>>,
    dependents: Vec<Vec<u32>>,
}

impl Graph {
    fn new<D: AsRef<[usize]>>(dependencies: &[D], included: &[usize]) -> Self {
        const NONE: u32 = u32::MAX;
        let mut id_of = vec![NONE; dependencies.len()];
        let mut ids = 0u32;
        for &node in included {
            if id_of[node] == NONE {
                id_of[node] = ids;
                ids += 1;
            }
        }
        let n = ids as usize;
        let mut deps: Vec<Vec<u32>> = vec![Vec::new(); n];
        let mut dependents: Vec<Vec<u32>> = vec![Vec::new(); n];
        for (node, node_deps) in dependencies.iter().enumerate() {
            let from = id_of[node];
            if from == NONE {
                continue;
            }
            for &dep in node_deps.as_ref() {
                let to = id_of[dep];
                // Keep the first of duplicate edges so the search order follows
                // the order dependencies were given in.
                if to != NONE && !deps[from as usize].contains(&to) {
                    deps[from as usize].push(to);
                    dependents[to as usize].push(from);
                }
            }
        }
        Self { deps, dependents }
    }
}

/// Breadth-first search for the cycle through a node, with buffers reused
/// across searches so each search costs only the component it explores.
struct CycleSearch {
    /// Search number that last reached each node.
    seen: Vec<u32>,
    /// Node each node was first reached from.
    parent: Vec<u32>,
    /// Number of edges from the start to each node.
    distance: Vec<u32>,
    queue: Vec<u32>,
    generation: u32,
}

impl CycleSearch {
    fn new(n: usize) -> Self {
        Self {
            seen: vec![0; n],
            parent: vec![0; n],
            distance: vec![0; n],
            queue: Vec::new(),
            generation: 0,
        }
    }

    /// The longest of the shortest cycles from `start` back to itself
    /// through nodes that are not removed, or an empty list when there is
    /// none. Among cycles of equal length, the one found first wins.
    fn find(
        &mut self,
        start: u32,
        deps: &[Vec<u32>],
        removed: &[bool],
        component: &[u32],
    ) -> Vec<u32> {
        self.generation += 1;
        let generation = self.generation;
        self.queue.clear();
        self.queue.push(start);
        self.distance[start as usize] = 0;
        // The node whose edge back to `start` closes the longest cycle.
        let mut closing: Option<u32> = None;
        let mut head = 0;
        while let Some(&id) = self.queue.get(head) {
            head += 1;
            for &to in &deps[id as usize] {
                if to == start {
                    if closing
                        .is_none_or(|c| self.distance[id as usize] > self.distance[c as usize])
                    {
                        closing = Some(id);
                    }
                    continue;
                }
                let t = to as usize;
                if removed[t]
                    || self.seen[t] == generation
                    || component[t] != component[start as usize]
                {
                    continue;
                }
                self.seen[t] = generation;
                self.parent[t] = id;
                self.distance[t] = self.distance[id as usize] + 1;
                self.queue.push(to);
            }
        }

        let Some(mut node) = closing else { return Vec::new() };
        let mut cycle = Vec::with_capacity(self.distance[node as usize] as usize + 1);
        while node != start {
            cycle.push(node);
            node = self.parent[node as usize];
        }
        cycle.push(start);
        cycle.reverse();
        cycle
    }
}

/// The strongly connected components of a graph.
#[derive(Debug)]
pub(crate) struct Components {
    /// Component of each node, or `u32::MAX` for a skipped node. Components
    /// are numbered in reverse topological order: a component comes after
    /// every component it has an edge to.
    pub of: Vec<u32>,
    /// Number of nodes in each component.
    pub sizes: Vec<u32>,
}

impl Components {
    /// Whether a cycle through `id` can exist: its component has another
    /// node, or it depends on itself.
    fn may_hold_cycle(&self, id: usize, deps: &[u32]) -> bool {
        self.sizes[self.of[id] as usize] >= 2 || deps.iter().any(|&d| d as usize == id)
    }
}

/// Tarjan's algorithm over nodes `0..n`, iterative so that long chains cannot
/// overflow the stack. Nodes for which `skip` returns true, and edges to them,
/// are left out.
pub(crate) fn strongly_connected<I>(
    n: usize,
    mut successors: impl FnMut(usize) -> I,
    skip: impl Fn(usize) -> bool,
) -> Components
where
    I: Iterator<Item = usize>,
{
    const NONE: u32 = u32::MAX;
    let mut index = vec![NONE; n];
    let mut low = vec![0u32; n];
    let mut on_stack = vec![false; n];
    let mut stack: Vec<usize> = Vec::new();
    let mut of = vec![NONE; n];
    let mut sizes: Vec<u32> = Vec::new();
    let mut next_index = 0u32;
    // Depth-first search frames: a node and its remaining successors.
    let mut frames: Vec<(usize, I)> = Vec::new();

    for root in 0..n {
        if skip(root) || index[root] != NONE {
            continue;
        }
        let mut enter = Some(root);
        loop {
            if let Some(v) = enter.take() {
                index[v] = next_index;
                low[v] = next_index;
                next_index += 1;
                stack.push(v);
                on_stack[v] = true;
                frames.push((v, successors(v)));
            }
            let Some((v, next)) = frames.last_mut() else { break };
            let v = *v;
            if let Some(w) = next.next() {
                if skip(w) {
                    continue;
                }
                if index[w] == NONE {
                    enter = Some(w);
                } else if on_stack[w] {
                    low[v] = low[v].min(index[w]);
                }
                continue;
            }
            frames.pop();
            if let Some(&(parent, _)) = frames.last() {
                low[parent] = low[parent].min(low[v]);
            }
            if low[v] == index[v] {
                let component = len_u32(sizes.len());
                let mut size = 0;
                loop {
                    let w = stack.pop().expect("a component's root is on the stack");
                    on_stack[w] = false;
                    of[w] = component;
                    size += 1;
                    if w == v {
                        break;
                    }
                }
                sizes.push(size);
            }
        }
    }
    Components { of, sizes }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs the sequencer on a graph of named nodes, like pnpm's tests.
    fn run(
        graph: &[(&'static str, &[&'static str])],
        included: &[&'static str],
    ) -> (Vec<&'static str>, Vec<Vec<&'static str>>) {
        let names: Vec<&str> = graph.iter().map(|(n, _)| *n).collect();
        let id = |name: &str| names.iter().position(|n| *n == name).expect("known node");
        let deps: Vec<Vec<usize>> =
            graph.iter().map(|(_, deps)| deps.iter().map(|d| id(d)).collect()).collect();
        let included: Vec<usize> = included.iter().map(|n| id(n)).collect();
        let s = sequence(&deps, &included);
        let name = |i: usize| names[i];
        (
            s.order.into_iter().map(name).collect(),
            s.cycles.into_iter().map(|c| c.into_iter().map(name).collect()).collect(),
        )
    }

    fn run_all(
        graph: &[(&'static str, &[&'static str])],
    ) -> (Vec<&'static str>, Vec<Vec<&'static str>>) {
        let all: Vec<&str> = graph.iter().map(|(n, _)| *n).collect();
        run(graph, &all)
    }

    // The cases below follow pnpm's graph-sequencer test suite.

    #[test]
    fn empty_graph() {
        assert_eq!(run_all(&[]), (vec![], vec![]));
    }

    #[test]
    fn single_node() {
        assert_eq!(run_all(&[("a", &[])]), (vec!["a"], vec![]));
    }

    #[test]
    fn independent_nodes_keep_their_order() {
        assert_eq!(run_all(&[("a", &[]), ("b", &[])]), (vec!["a", "b"], vec![]));
    }

    #[test]
    fn dependencies_come_first() {
        assert_eq!(run_all(&[("a", &["b"]), ("b", &[])]), (vec!["b", "a"], vec![]));
        assert_eq!(
            run_all(&[("a", &["b"]), ("b", &["c"]), ("c", &[]), ("d", &["c"])]),
            (vec!["c", "b", "d", "a"], vec![])
        );
    }

    #[test]
    fn two_node_cycle() {
        assert_eq!(
            run_all(&[("a", &["b"]), ("b", &["a"])]),
            (vec!["a", "b"], vec![vec!["a", "b"]])
        );
    }

    #[test]
    fn self_dependency_is_a_cycle() {
        assert_eq!(run_all(&[("a", &["a"]), ("b", &[])]), (vec!["b", "a"], vec![vec!["a"]]));
    }

    #[test]
    fn cycle_with_a_tail() {
        // d depends on the cycle a -> b -> c -> a.
        assert_eq!(
            run_all(&[("a", &["b"]), ("b", &["c"]), ("c", &["a"]), ("d", &["a"])]),
            (vec!["a", "b", "c", "d"], vec![vec!["a", "b", "c"]])
        );
    }

    #[test]
    fn two_separate_cycles() {
        assert_eq!(
            run_all(&[("a", &["b"]), ("b", &["a"]), ("c", &["d"]), ("d", &["c"])]),
            (vec!["a", "b", "c", "d"], vec![vec!["a", "b"], vec!["c", "d"]])
        );
    }

    #[test]
    fn longest_of_the_shortest_cycles_wins() {
        // a is on a -> b -> a and on a -> c -> d -> a; the longer one is kept,
        // which leaves b to be sequenced after it.
        assert_eq!(
            run_all(&[("a", &["b", "c"]), ("b", &["a"]), ("c", &["d"]), ("d", &["a"])]),
            (vec!["a", "c", "d", "b"], vec![vec!["a", "c", "d"]])
        );
    }

    #[test]
    fn cycles_left_after_breaking_one_are_found_later() {
        // Breaking a -> b -> a leaves c <-> d, which hang off b.
        let (order, cycles) =
            run_all(&[("a", &["b"]), ("b", &["a", "c"]), ("c", &["d"]), ("d", &["c"])]);
        assert_eq!(cycles, vec![vec!["a", "b"], vec!["c", "d"]]);
        assert_eq!(order, vec!["a", "b", "c", "d"]);
    }

    #[test]
    fn nodes_outside_included_are_ignored() {
        // b is left out, so a no longer waits on it and the cycle through it
        // disappears.
        assert_eq!(
            run(&[("a", &["b"]), ("b", &["a"]), ("c", &["a"])], &["a", "c"]),
            (vec!["a", "c"], vec![])
        );
    }

    #[test]
    fn included_order_breaks_ties() {
        let graph: &[(&str, &[&str])] = &[("a", &["b"]), ("b", &["a"]), ("c", &[])];
        assert_eq!(run(graph, &["c", "b", "a"]), (vec!["c", "b", "a"], vec![vec!["b", "a"]]));
    }

    #[test]
    fn duplicate_edges_count_once() {
        assert_eq!(run_all(&[("a", &["b", "b"]), ("b", &[])]), (vec!["b", "a"], vec![]));
    }

    #[test]
    fn long_cycle_does_not_overflow() {
        let n = 100_000;
        let deps: Vec<Vec<usize>> = (0..n).map(|i| vec![(i + 1) % n]).collect();
        let included: Vec<usize> = (0..n).collect();
        let s = sequence(&deps, &included);
        assert_eq!(s.cycles.len(), 1);
        assert_eq!(s.cycles[0], included);
    }

    /// The port gives the same order and cycles as pnpm's implementation on
    /// 400 random graphs, generated by `tests/fixtures/graph-sequencer/gen.mjs`.
    #[test]
    fn matches_pnpm_on_random_graphs() {
        #[derive(serde::Deserialize)]
        struct Case {
            deps: Vec<Vec<usize>>,
            included: Vec<usize>,
            order: Vec<usize>,
            cycles: Vec<Vec<usize>>,
        }
        let cases: Vec<Case> =
            serde_json::from_str(include_str!("../tests/fixtures/graph-sequencer/cases.json"))
                .expect("valid fixture");
        assert_eq!(cases.len(), 400);
        for (i, case) in cases.iter().enumerate() {
            let s = sequence(&case.deps, &case.included);
            assert_eq!((&s.order, &s.cycles), (&case.order, &case.cycles), "case {i}");
        }
    }

    #[test]
    fn components_are_in_reverse_topological_order() {
        // 0 -> 1 <-> 2 -> 3
        let succ: Vec<Vec<usize>> = vec![vec![1], vec![2], vec![1, 3], vec![]];
        let c = strongly_connected(4, |v| succ[v].iter().copied(), |_| false);
        assert_eq!(c.of[1], c.of[2]);
        assert_eq!(c.sizes[c.of[1] as usize], 2);
        assert!(c.of[3] < c.of[1] && c.of[1] < c.of[0]);
    }

    #[test]
    fn skipped_nodes_split_components() {
        // 0 -> 1 -> 2 -> 0, with 2 skipped.
        let succ: Vec<Vec<usize>> = vec![vec![1], vec![2], vec![0]];
        let c = strongly_connected(3, |v| succ[v].iter().copied(), |v| v == 2);
        assert_ne!(c.of[0], c.of[1]);
        assert_eq!(c.of[2], u32::MAX);
    }
}
