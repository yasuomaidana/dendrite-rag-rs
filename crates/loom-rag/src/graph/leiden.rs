//! Leiden community detection (Traag, Waltman & van Eck, 2019) maximizing modularity with a
//! resolution parameter, on a weighted undirected graph (`docs/specs/graph.md`).
//!
//! Each level runs fast local moving, then refines every community so that only
//! well-connected subsets are merged (which guarantees connected communities), then
//! aggregates the refined communities into nodes. The refinement picks the best merge
//! instead of a random one, so results are deterministic for a given node order.

use std::collections::{BTreeMap, HashMap, VecDeque};

/// A weighted undirected graph: `adj[i]` lists `(j, w)` for every edge, in both
/// directions; self-loops are kept separately.
#[derive(Debug, Clone)]
struct Graph {
    adj: Vec<Vec<(u32, f64)>>,
    self_loops: Vec<f64>,
    /// Node degree: incident edge weights plus twice the self-loop.
    degree: Vec<f64>,
    /// Twice the total edge weight.
    two_m: f64,
}

impl Graph {
    fn new(adj: Vec<Vec<(u32, f64)>>, self_loops: Vec<f64>) -> Self {
        let degree: Vec<f64> = adj
            .iter()
            .zip(&self_loops)
            .map(|(e, s)| e.iter().map(|(_, w)| w).sum::<f64>() + 2.0 * s)
            .collect();
        let two_m = degree.iter().sum();
        Self {
            adj,
            self_loops,
            degree,
            two_m,
        }
    }

    fn len(&self) -> usize {
        self.adj.len()
    }
}

/// Communities of `adj` (an undirected adjacency list as built by
/// [`crate::graph::CodeGraph::undirected`]). Returns one community index per node,
/// numbered `0..` in order of each community's first node.
pub fn leiden(adj: &[Vec<(u32, f32)>], resolution: f64) -> Vec<u32> {
    let n = adj.len();
    let mut graph = Graph::new(
        adj.iter()
            .enumerate()
            .map(|(i, e)| {
                e.iter()
                    .filter(|(j, w)| *j as usize != i && *w > 0.0)
                    .map(|&(j, w)| (j, w as f64))
                    .collect()
            })
            .collect(),
        vec![0.0; n],
    );
    if graph.two_m == 0.0 {
        return (0..n as u32).collect();
    }
    // `members[i]`: the original nodes inside aggregated node `i`.
    let mut members: Vec<Vec<u32>> = (0..n as u32).map(|i| vec![i]).collect();
    let mut partition: Vec<u32> = (0..n as u32).collect();
    // Every level aggregates to fewer nodes, so this ends.
    loop {
        local_moving(&graph, &mut partition, resolution);
        let communities = renumber(&mut partition);
        if communities == graph.len() {
            break;
        }
        let mut refined = refine(&graph, &partition, resolution);
        let mut count = renumber(&mut refined);
        if count == graph.len() {
            // Nothing was well connected enough to merge: aggregate whole communities.
            refined = partition.clone();
            count = communities;
        }
        // One node per refined community, starting in its parent community.
        let mut next_partition = vec![0u32; count];
        let mut next_members: Vec<Vec<u32>> = vec![Vec::new(); count];
        for v in 0..graph.len() {
            let r = refined[v] as usize;
            next_partition[r] = partition[v];
            next_members[r].extend_from_slice(&members[v]);
        }
        graph = aggregate(&graph, &refined, count);
        members = next_members;
        partition = next_partition;
    }
    let mut out = vec![0u32; n];
    for (agg, nodes) in members.iter().enumerate() {
        for &v in nodes {
            out[v as usize] = partition[agg];
        }
    }
    renumber(&mut out);
    out
}

/// Weights from `v` to each neighbouring community.
fn community_links(g: &Graph, partition: &[u32], v: usize) -> BTreeMap<u32, f64> {
    let mut links = BTreeMap::new();
    for &(u, w) in &g.adj[v] {
        *links.entry(partition[u as usize]).or_insert(0.0) += w;
    }
    links
}

/// Moves nodes to the neighbouring community with the largest modularity gain until no
/// move improves it, revisiting only the neighbours of moved nodes.
fn local_moving(g: &Graph, partition: &mut [u32], gamma: f64) {
    let n = g.len();
    let mut totals: HashMap<u32, f64> = HashMap::new();
    for (&c, &k) in partition.iter().zip(&g.degree) {
        *totals.entry(c).or_insert(0.0) += k;
    }
    // An id no community uses, for moving a node out on its own.
    let mut free = partition.iter().max().map_or(0, |m| m + 1);
    let mut queue: VecDeque<usize> = (0..n).collect();
    let mut queued = vec![true; n];
    while let Some(v) = queue.pop_front() {
        queued[v] = false;
        let current = partition[v];
        let k = g.degree[v];
        let links = community_links(g, partition, v);
        *totals.get_mut(&current).expect("community total") -= k;
        let gain = |c: u32, w: f64, totals: &HashMap<u32, f64>| {
            w - gamma * k * totals.get(&c).copied().unwrap_or(0.0) / g.two_m
        };
        let mut best = current;
        let mut best_gain = gain(current, links.get(&current).copied().unwrap_or(0.0), &totals);
        for (&c, &w) in &links {
            let gn = gain(c, w, &totals);
            if gn > best_gain + 1e-12 {
                best = c;
                best_gain = gn;
            }
        }
        // Alone in a new community (gain 0) when every neighbour is worse.
        if best_gain < -1e-12 && totals.get(&current).copied().unwrap_or(0.0) > 0.0 {
            best = free;
            free += 1;
        }
        *totals.entry(best).or_insert(0.0) += k;
        if best != current {
            partition[v] = best;
            for &(u, _) in &g.adj[v] {
                let u = u as usize;
                if !queued[u] && partition[u] != best {
                    queued[u] = true;
                    queue.push_back(u);
                }
            }
        }
    }
}

/// Splits each community into well-connected subcommunities: starting from singletons,
/// a node that is well connected to its community joins the well-connected
/// subcommunity (within the same community) with the best non-negative gain.
fn refine(g: &Graph, partition: &[u32], gamma: f64) -> Vec<u32> {
    let mut st = Refinement::new(g, partition, gamma);
    for (v, c) in partition.iter().enumerate() {
        if st.sub_size[st.refined[v] as usize] != 1 {
            continue; // already merged into another node's subcommunity
        }
        let total_c = st.comm_total[c];
        if !st.well_connected(total_c, st.sub_external[v], g.degree[v]) {
            continue;
        }
        let links = st.community_links(v);
        let best = st.best_join(v, total_c, &links);
        if best != st.refined[v] {
            st.merge(v, best, links[&best]);
        }
    }
    st.refined
}

/// The state of [`refine`]: each node's subcommunity and what the well-connectedness test needs.
struct Refinement<'a> {
    g: &'a Graph,
    partition: &'a [u32],
    gamma: f64,
    refined: Vec<u32>,
    sub_total: Vec<f64>,
    sub_size: Vec<usize>,
    comm_total: HashMap<u32, f64>,
    /// Weight from each subcommunity to the rest of its community, kept current.
    sub_external: Vec<f64>,
}

impl<'a> Refinement<'a> {
    fn new(g: &'a Graph, partition: &'a [u32], gamma: f64) -> Self {
        let n = g.len();
        let mut comm_total: HashMap<u32, f64> = HashMap::new();
        for (&c, &k) in partition.iter().zip(&g.degree) {
            *comm_total.entry(c).or_insert(0.0) += k;
        }
        let sub_external = (0..n)
            .map(|v| {
                g.adj[v]
                    .iter()
                    .filter(|(u, _)| partition[*u as usize] == partition[v])
                    .map(|(_, w)| w)
                    .sum()
            })
            .collect();
        Self {
            g,
            partition,
            gamma,
            refined: (0..n as u32).collect(),
            sub_total: g.degree.clone(),
            sub_size: vec![1; n],
            comm_total,
            sub_external,
        }
    }

    /// Whether a set with `ext` weight to the rest of a community of `total_c` and degree `tot` is well connected.
    fn well_connected(&self, total_c: f64, ext: f64, tot: f64) -> bool {
        ext >= self.gamma * tot * (total_c - tot) / self.g.two_m
    }

    /// The weight from `v` to each subcommunity of its own community.
    fn community_links(&self, v: usize) -> BTreeMap<u32, f64> {
        let c = self.partition[v];
        let mut links: BTreeMap<u32, f64> = BTreeMap::new();
        for &(u, w) in &self.g.adj[v] {
            if self.partition[u as usize] == c {
                *links.entry(self.refined[u as usize]).or_insert(0.0) += w;
            }
        }
        links
    }

    /// The well-connected subcommunity of `links` with the best positive gain for `v`, or its own.
    fn best_join(&self, v: usize, total_c: f64, links: &BTreeMap<u32, f64>) -> u32 {
        let k = self.g.degree[v];
        let own = self.refined[v];
        let mut best = own;
        let mut best_gain = 0.0;
        for (&s, &w) in links {
            let (ext, tot) = (self.sub_external[s as usize], self.sub_total[s as usize]);
            if s == own || !self.well_connected(total_c, ext, tot) {
                continue;
            }
            let gain = w - self.gamma * k * tot / self.g.two_m;
            if gain > best_gain + 1e-12 {
                best = s;
                best_gain = gain;
            }
        }
        best
    }

    /// Moves `v` into subcommunity `best`, joined to it by `w_vs`.
    fn merge(&mut self, v: usize, best: u32, w_vs: f64) {
        let own = self.refined[v];
        let b = best as usize;
        self.refined[v] = best;
        self.sub_total[b] += self.g.degree[v];
        self.sub_size[b] += 1;
        self.sub_size[own as usize] = 0;
        // External weight of the merged set: both sides' minus the edges between them.
        self.sub_external[b] = self.sub_external[b] + self.sub_external[v] - 2.0 * w_vs;
    }
}

/// Collapses each community of `ids` into one node, summing edge weights.
fn aggregate(g: &Graph, ids: &[u32], count: usize) -> Graph {
    let mut adj: Vec<BTreeMap<u32, f64>> = vec![BTreeMap::new(); count];
    let mut self_loops = vec![0.0; count];
    for v in 0..g.len() {
        let a = ids[v];
        self_loops[a as usize] += g.self_loops[v];
        for &(u, w) in &g.adj[v] {
            let b = ids[u as usize];
            if a == b {
                // Each internal edge is seen from both ends.
                self_loops[a as usize] += w / 2.0;
            } else {
                *adj[a as usize].entry(b).or_insert(0.0) += w;
            }
        }
    }
    Graph::new(
        adj.into_iter().map(|m| m.into_iter().collect()).collect(),
        self_loops,
    )
}

/// Renumbers community ids to `0..count` in order of first appearance; returns `count`.
fn renumber(ids: &mut [u32]) -> usize {
    let mut map: HashMap<u32, u32> = HashMap::new();
    for id in ids.iter_mut() {
        let next = map.len() as u32;
        *id = *map.entry(*id).or_insert(next);
    }
    map.len()
}

/// Newman-Girvan modularity of `communities` on `adj` with resolution `gamma`.
pub fn modularity(adj: &[Vec<(u32, f32)>], communities: &[u32], gamma: f64) -> f64 {
    let degree: Vec<f64> = adj
        .iter()
        .map(|e| e.iter().map(|(_, w)| *w as f64).sum())
        .collect();
    let two_m: f64 = degree.iter().sum();
    if two_m == 0.0 {
        return 0.0;
    }
    let mut internal: HashMap<u32, f64> = HashMap::new();
    let mut totals: HashMap<u32, f64> = HashMap::new();
    for (v, e) in adj.iter().enumerate() {
        *totals.entry(communities[v]).or_insert(0.0) += degree[v];
        for &(u, w) in e {
            if communities[u as usize] == communities[v] {
                *internal.entry(communities[v]).or_insert(0.0) += w as f64;
            }
        }
    }
    totals
        .iter()
        .map(|(c, t)| internal.get(c).copied().unwrap_or(0.0) / two_m - gamma * (t / two_m).powi(2))
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn undirected(n: usize, edges: &[(u32, u32, f32)]) -> Vec<Vec<(u32, f32)>> {
        let mut adj = vec![Vec::new(); n];
        for &(a, b, w) in edges {
            adj[a as usize].push((b, w));
            adj[b as usize].push((a, w));
        }
        adj
    }

    /// `k` cliques of `size` nodes, consecutive cliques joined by one edge.
    fn ring_of_cliques(k: usize, size: usize) -> Vec<Vec<(u32, f32)>> {
        let mut edges = Vec::new();
        for c in 0..k {
            let base = (c * size) as u32;
            for i in 0..size as u32 {
                for j in i + 1..size as u32 {
                    edges.push((base + i, base + j, 1.0));
                }
            }
            let next = (((c + 1) % k) * size) as u32;
            edges.push((base, next + 1, 1.0));
        }
        undirected(k * size, &edges)
    }

    fn connected(adj: &[Vec<(u32, f32)>], comm: &[u32], c: u32) -> bool {
        let nodes: Vec<usize> = (0..adj.len()).filter(|&v| comm[v] == c).collect();
        let mut seen = vec![false; adj.len()];
        let mut stack = vec![nodes[0]];
        seen[nodes[0]] = true;
        let mut count = 0;
        while let Some(v) = stack.pop() {
            count += 1;
            for &(u, _) in &adj[v] {
                let u = u as usize;
                if comm[u] == c && !seen[u] {
                    seen[u] = true;
                    stack.push(u);
                }
            }
        }
        count == nodes.len()
    }

    #[test]
    fn finds_planted_cliques() {
        let adj = ring_of_cliques(6, 5);
        let comm = leiden(&adj, 1.0);
        for c in 0..6 {
            let first = comm[c * 5];
            assert!(
                (0..5).all(|i| comm[c * 5 + i] == first),
                "clique {c} split: {comm:?}"
            );
        }
        let distinct: std::collections::HashSet<_> = comm.iter().collect();
        assert_eq!(distinct.len(), 6, "{comm:?}");
        assert!(modularity(&adj, &comm, 1.0) > 0.6);
    }

    #[test]
    fn communities_are_connected_and_deterministic() {
        // Two dense halves joined weakly, plus noise edges.
        let mut edges = Vec::new();
        for i in 0..40u32 {
            for j in i + 1..40 {
                let same = (i < 20) == (j < 20);
                if (same && (i * 7 + j * 3) % 4 == 0) || (!same && (i + j) % 37 == 0) {
                    edges.push((i, j, 1.0));
                }
            }
        }
        let adj = undirected(40, &edges);
        let a = leiden(&adj, 1.0);
        assert_eq!(a, leiden(&adj, 1.0));
        let count = *a.iter().max().unwrap() + 1;
        for c in 0..count {
            assert!(connected(&adj, &a, c), "community {c} is disconnected: {a:?}");
        }
        assert!(modularity(&adj, &a, 1.0) > 0.2);
    }

    #[test]
    fn isolated_nodes_stay_alone_and_resolution_matters() {
        let adj = undirected(5, &[(0, 1, 1.0), (1, 2, 1.0)]);
        let comm = leiden(&adj, 1.0);
        assert_eq!(comm[0], comm[2]);
        assert_ne!(comm[3], comm[4]);
        assert!(leiden(&[Vec::new(), Vec::new()], 1.0) == [0, 1]);

        let adj = ring_of_cliques(4, 4);
        let fine = leiden(&adj, 4.0);
        let coarse = leiden(&adj, 0.05);
        let n = |c: &[u32]| *c.iter().max().unwrap() + 1;
        assert!(n(&fine) > n(&coarse), "fine {fine:?} coarse {coarse:?}");
    }
}
