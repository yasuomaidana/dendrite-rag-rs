//! Shortest paths over the code graph (breadth-first, optionally against edge direction).

use super::*;

/// A node on a found path with the `(kind, reversed)` edge that reached it.
type Hop = (u32, Option<(EdgeKind, bool)>);

/// Breadth-first search from any of `src` to any of `dst`; each returned step carries the
/// edge `(kind, reversed)` that reached it.
pub(super) fn bfs(
    src: &[u32],
    dst: &[u32],
    max_hops: usize,
    next: impl Fn(u32) -> Vec<(u32, EdgeKind, bool)>,
) -> Option<Vec<Hop>> {
    let targets: HashSet<u32> = dst.iter().copied().collect();
    let mut prev: HashMap<u32, Option<(u32, EdgeKind, bool)>> = HashMap::new();
    let mut queue: VecDeque<(u32, usize)> = VecDeque::new();
    for &s in src {
        prev.insert(s, None);
        queue.push_back((s, 0));
    }
    while let Some((v, depth)) = queue.pop_front() {
        if targets.contains(&v) {
            let mut path = vec![(v, prev[&v].map(|(_, k, r)| (k, r)))];
            let mut cur = v;
            while let Some((p, _, _)) = prev[&cur] {
                path.push((p, prev[&p].map(|(_, k, r)| (k, r))));
                cur = p;
            }
            path.reverse();
            return Some(path);
        }
        if depth == max_hops {
            continue;
        }
        for (u, k, r) in next(v) {
            if let std::collections::hash_map::Entry::Vacant(e) = prev.entry(u) {
                e.insert(Some((v, k, r)));
                queue.push_back((u, depth + 1));
            }
        }
    }
    None
}

/// File → (file, kind) for every edge pointing at it.
pub(super) fn incoming_edges(g: &CodeGraph) -> Vec<Vec<(u32, EdgeKind)>> {
    let mut incoming = vec![Vec::new(); g.file_count()];
    for f in 0..g.file_count() as u32 {
        for (t, k, _) in g.file_edges(f) {
            incoming[t as usize].push((f, k));
        }
    }
    incoming
}
