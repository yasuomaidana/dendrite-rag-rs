//! Approximate nearest-neighbour index using Hierarchical Navigable Small World graphs.
//!
//! Wraps a [`FlatIndex`] to keep int8 vector storage in `vectors.bin` while maintaining
//! an in-memory graph index persisted to `vectors.hnsw`.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use crate::vector::{FlatIndex, VectorIndex, dot_i8, quantize};

pub const DEFAULT_M: usize = 16;
pub const DEFAULT_M0: usize = 32;
pub const DEFAULT_EF_CONSTRUCTION: usize = 200;
/// 128 keeps recall@10 of the engine's rescored 40-candidate shortlist above 0.95 on
/// 20k vectors (64 gives 0.82 on uniform data; see `recall_report`).
pub const DEFAULT_EF_SEARCH: usize = 128;

const HNSW_MAGIC: &[u8; 4] = b"DRH1";

#[derive(Debug, Clone, Copy, PartialEq)]
struct OrderedF32(f32);

impl Eq for OrderedF32 {}

impl PartialOrd for OrderedF32 {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for OrderedF32 {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.total_cmp(&other.0)
    }
}

/// A node in the HNSW graph.
#[derive(Debug, Clone)]
pub struct HnswNode {
    pub id: String,
    pub level: usize,
    pub deleted: bool,
    /// Neighbors at each level from 0..=level.
    pub neighbors: Vec<Vec<u32>>,
    /// Preserved vector for deleted nodes serving as tombstones during routing.
    pub tombstone_vector: Option<(Vec<i8>, f32)>,
}

/// Approximate nearest-neighbour index implementing HNSW.
#[derive(Debug, Clone)]
pub struct HnswIndex {
    flat: FlatIndex,
    m: usize,
    m0: usize,
    ef_construction: usize,
    ef_search: usize,
    max_level: usize,
    entry_point: Option<u32>,
    nodes: Vec<HnswNode>,
    id_to_node: HashMap<String, u32>,
    deleted_count: usize,
    rng_state: u64,
    hnsw_path: Option<PathBuf>,
}

impl Default for HnswIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl HnswIndex {
    pub fn new() -> Self {
        Self::with_flat(FlatIndex::new())
    }

    pub fn with_flat(flat: FlatIndex) -> Self {
        Self::with_params(flat, DEFAULT_M, DEFAULT_EF_CONSTRUCTION, DEFAULT_EF_SEARCH)
    }

    pub fn with_params(flat: FlatIndex, m: usize, ef_construction: usize, ef_search: usize) -> Self {
        let m0 = 2 * m;
        let mut idx = Self {
            flat,
            m,
            m0,
            ef_construction,
            ef_search,
            max_level: 0,
            entry_point: None,
            nodes: Vec::new(),
            id_to_node: HashMap::new(),
            deleted_count: 0,
            rng_state: 0x123456789abcdef0,
            hnsw_path: None,
        };
        if idx.flat.len() > 0 {
            idx.rebuild();
        }
        idx
    }

    pub fn set_ef_search(&mut self, ef_search: usize) {
        self.ef_search = ef_search;
    }

    pub fn ef_search(&self) -> usize {
        self.ef_search
    }

    pub fn set_path(&mut self, path: impl Into<PathBuf>) {
        let p = path.into();
        self.hnsw_path = Some(p.with_file_name("vectors.hnsw"));
        self.flat.set_path(p);
    }

    pub fn with_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.set_path(path);
        self
    }

    pub fn flat(&self) -> &FlatIndex {
        &self.flat
    }

    pub fn flat_mut(&mut self) -> &mut FlatIndex {
        &mut self.flat
    }

    fn random_level(&mut self) -> usize {
        let mut f: f64 = 0.0;
        while f == 0.0 {
            self.rng_state = self
                .rng_state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            f = ((self.rng_state >> 11) as f64) / ((1u64 << 53) as f64);
        }
        let ml = 1.0 / (self.m as f64).ln();
        let lvl = (-f.ln() * ml).floor() as usize;
        lvl.min(32)
    }

    fn node_vector_data(&self, node_id: u32) -> (&[i8], f32) {
        let node = &self.nodes[node_id as usize];
        if let Some((v, scale)) = &node.tombstone_vector {
            (v.as_slice(), *scale)
        } else {
            let row = self
                .flat
                .row_of(&node.id)
                .expect("active node must exist in flat index");
            self.flat.vector_at(row)
        }
    }

    fn dist_to_q(&self, q_i8: &[i8], q_scale: f32, node_id: u32) -> f32 {
        let (v, v_scale) = self.node_vector_data(node_id);
        let sim = dot_i8(q_i8, v) as f32 * q_scale * v_scale;
        1.0 - sim
    }

    fn dist_nodes(&self, a: u32, b: u32) -> f32 {
        let (va, sa) = self.node_vector_data(a);
        let (vb, sb) = self.node_vector_data(b);
        let sim = dot_i8(va, vb) as f32 * sa * sb;
        1.0 - sim
    }

    fn search_layer(
        &self,
        q_i8: &[i8],
        q_scale: f32,
        entry_points: &[u32],
        ef: usize,
        level: usize,
    ) -> BinaryHeap<(OrderedF32, u32)> {
        let mut visited = HashSet::with_capacity(ef * 2);
        let mut candidates = BinaryHeap::new();
        let mut w = BinaryHeap::new();

        for &ep in entry_points {
            let d = self.dist_to_q(q_i8, q_scale, ep);
            visited.insert(ep);
            candidates.push(Reverse((OrderedF32(d), ep)));
            w.push((OrderedF32(d), ep));
        }

        while let Some(Reverse((c_dist, c))) = candidates.pop() {
            if let Some(&(furthest_dist, _)) = w.peek()
                && c_dist > furthest_dist
                && w.len() >= ef
            {
                break;
            }

            if let Some(neighbors) = self.nodes[c as usize].neighbors.get(level) {
                for &e in neighbors {
                    if visited.insert(e) {
                        let d = self.dist_to_q(q_i8, q_scale, e);
                        let can_insert = if w.len() < ef {
                            true
                        } else if let Some(&(furthest_dist, _)) = w.peek() {
                            OrderedF32(d) < furthest_dist
                        } else {
                            false
                        };

                        if can_insert {
                            candidates.push(Reverse((OrderedF32(d), e)));
                            w.push((OrderedF32(d), e));
                            if w.len() > ef {
                                w.pop();
                            }
                        }
                    }
                }
            }
        }

        w
    }

    fn select_neighbors_heuristic(
        &self,
        target: u32,
        candidates: &[(OrderedF32, u32)],
        m: usize,
    ) -> Vec<u32> {
        let mut seen = HashSet::new();
        let mut sorted: Vec<(OrderedF32, u32)> = Vec::with_capacity(candidates.len());
        for &(d, id) in candidates {
            if id != target && seen.insert(id) {
                sorted.push((d, id));
            }
        }
        sorted.sort_by_key(|a| a.0);

        let mut result: Vec<u32> = Vec::with_capacity(m);
        let mut skipped: Vec<u32> = Vec::new();

        for &(dist_to_target, c) in &sorted {
            if result.len() >= m {
                break;
            }
            let mut keep = true;
            for &r in &result {
                let dist_c_r = self.dist_nodes(c, r);
                if dist_c_r < dist_to_target.0 {
                    keep = false;
                    break;
                }
            }
            if keep {
                result.push(c);
            } else {
                skipped.push(c);
            }
        }

        for s in skipped {
            if result.len() >= m {
                break;
            }
            if !result.contains(&s) {
                result.push(s);
            }
        }

        result
    }

    fn insert_graph_node(&mut self, id: &str, vector: &[f32]) {
        let node_id = self.nodes.len() as u32;
        let node_level = self.random_level();
        let (q_i8, q_scale) = quantize(vector);

        let new_node = HnswNode {
            id: id.to_string(),
            level: node_level,
            deleted: false,
            neighbors: vec![Vec::new(); node_level + 1],
            tombstone_vector: None,
        };

        let Some(mut curr) = self.entry_point else {
            self.entry_point = Some(node_id);
            self.max_level = node_level;
            self.nodes.push(new_node);
            self.id_to_node.insert(id.to_string(), node_id);
            return;
        };

        let max_l = self.max_level;
        let mut curr_dist = self.dist_to_q(&q_i8, q_scale, curr);

        for lc in (node_level + 1..=max_l).rev() {
            let mut changed = true;
            while changed {
                changed = false;
                if let Some(neighbors) = self.nodes[curr as usize].neighbors.get(lc) {
                    let mut best_neighbor = curr;
                    let mut best_dist = curr_dist;
                    for &neighbor in neighbors {
                        let d = self.dist_to_q(&q_i8, q_scale, neighbor);
                        if d < best_dist {
                            best_neighbor = neighbor;
                            best_dist = d;
                        }
                    }
                    if best_neighbor != curr {
                        curr = best_neighbor;
                        curr_dist = best_dist;
                        changed = true;
                    }
                }
            }
        }

        self.nodes.push(new_node);
        self.id_to_node.insert(id.to_string(), node_id);

        for lc in (0..=node_level.min(max_l)).rev() {
            let m_max = if lc == 0 { self.m0 } else { self.m };
            let w = self.search_layer(&q_i8, q_scale, &[curr], self.ef_construction, lc);
            let candidates: Vec<(OrderedF32, u32)> = w.into_vec();

            let neighbors = self.select_neighbors_heuristic(node_id, &candidates, m_max);
            self.nodes[node_id as usize].neighbors[lc] = neighbors.clone();

            for n in neighbors {
                let n_idx = n as usize;
                if self.nodes[n_idx].neighbors[lc].len() < m_max {
                    self.nodes[n_idx].neighbors[lc].push(node_id);
                } else {
                    let mut cands: Vec<(OrderedF32, u32)> = self.nodes[n_idx].neighbors[lc]
                        .iter()
                        .map(|&other| (OrderedF32(self.dist_nodes(n, other)), other))
                        .collect();
                    cands.push((OrderedF32(self.dist_nodes(n, node_id)), node_id));

                    let pruned = self.select_neighbors_heuristic(n, &cands, m_max);
                    self.nodes[n_idx].neighbors[lc] = pruned;
                }
            }

            if let Some(closest) = candidates.iter().min_by(|a, b| a.0.cmp(&b.0)) {
                curr = closest.1;
            }
        }

        if node_level > self.max_level {
            self.max_level = node_level;
            self.entry_point = Some(node_id);
        }
    }

    pub fn rebuild(&mut self) {
        let count = self.flat.len();
        self.nodes.clear();
        self.id_to_node.clear();
        self.entry_point = None;
        self.max_level = 0;
        self.deleted_count = 0;

        if count == 0 || self.flat.dim() == 0 {
            return;
        }

        let mut items = Vec::with_capacity(count);
        for row in 0..count {
            let id = self.flat.id_at(row).to_string();
            let v = self.flat.vector_f32(row);
            items.push((id, v));
        }

        for (id, v) in items {
            self.insert_graph_node(&id, &v);
        }
    }

    pub fn search_knn(&self, query: &[f32], k: usize) -> Vec<(String, f32)> {
        if self.is_empty() || k == 0 || query.len() != self.dim() {
            return Vec::new();
        }
        let Some(entry_point) = self.entry_point else {
            return Vec::new();
        };
        let (q_i8, q_scale) = quantize(query);
        let mut curr = entry_point;
        let mut curr_dist = self.dist_to_q(&q_i8, q_scale, curr);

        for lc in (1..=self.max_level).rev() {
            let mut changed = true;
            while changed {
                changed = false;
                if let Some(neighbors) = self.nodes[curr as usize].neighbors.get(lc) {
                    let mut best_neighbor = curr;
                    let mut best_dist = curr_dist;
                    for &neighbor in neighbors {
                        let d = self.dist_to_q(&q_i8, q_scale, neighbor);
                        if d < best_dist {
                            best_neighbor = neighbor;
                            best_dist = d;
                        }
                    }
                    if best_neighbor != curr {
                        curr = best_neighbor;
                        curr_dist = best_dist;
                        changed = true;
                    }
                }
            }
        }

        let ef = self.ef_search.max(k);
        let candidates = self.search_layer(&q_i8, q_scale, &[curr], ef, 0);

        let mut scored: Vec<(String, f32)> = candidates
            .into_iter()
            .filter_map(|(dist, node_id)| {
                let node = &self.nodes[node_id as usize];
                if node.deleted {
                    None
                } else {
                    let sim = 1.0 - dist.0;
                    Some((node.id.clone(), sim))
                }
            })
            .collect();

        scored.sort_by(|a, b| b.1.total_cmp(&a.1));
        if scored.len() > k {
            scored.truncate(k);
        }
        scored
    }

    pub fn save_hnsw(&self, path: &Path) -> std::io::Result<()> {
        for node in &self.nodes {
            if node.id.len() > u16::MAX as usize {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("id length {} exceeds u16::MAX", node.id.len()),
                ));
            }
        }

        let tmp = path.with_extension("tmp");
        {
            let mut file = std::fs::File::create(&tmp)?;
            let mut hasher = blake3::Hasher::new();
            {
                let mut w = std::io::BufWriter::new(&mut file);
                let mut write_hashed = |buf: &[u8]| -> std::io::Result<()> {
                    w.write_all(buf)?;
                    hasher.update(buf);
                    Ok(())
                };

                write_hashed(HNSW_MAGIC)?;
                write_hashed(&1u32.to_le_bytes())?;
                write_hashed(&(self.m as u32).to_le_bytes())?;
                write_hashed(&(self.ef_construction as u32).to_le_bytes())?;
                let ep = self.entry_point.map(|e| e as i64).unwrap_or(-1);
                write_hashed(&ep.to_le_bytes())?;
                write_hashed(&(self.max_level as u32).to_le_bytes())?;
                write_hashed(&(self.nodes.len() as u32).to_le_bytes())?;

                for node in &self.nodes {
                    write_hashed(&(node.id.len() as u16).to_le_bytes())?;
                    write_hashed(node.id.as_bytes())?;
                    write_hashed(&(node.level as u32).to_le_bytes())?;
                    write_hashed(&[if node.deleted { 1u8 } else { 0u8 }])?;

                    if node.deleted {
                        if let Some((v, scale)) = &node.tombstone_vector {
                            write_hashed(&1u8.to_le_bytes())?;
                            write_hashed(&(v.len() as u32).to_le_bytes())?;
                            write_hashed(&scale.to_le_bytes())?;
                            let raw_bytes: Vec<u8> = v.iter().map(|&x| x as u8).collect();
                            write_hashed(&raw_bytes)?;
                        } else {
                            write_hashed(&0u8.to_le_bytes())?;
                        }
                    }

                    for lc in 0..=node.level {
                        let neighbors = &node.neighbors[lc];
                        write_hashed(&(neighbors.len() as u32).to_le_bytes())?;
                        for &n in neighbors {
                            write_hashed(&n.to_le_bytes())?;
                        }
                    }
                }

                let checksum = hasher.finalize();
                w.write_all(checksum.as_bytes())?;
                w.flush()?;
            }
            file.sync_all()?;
        }
        std::fs::rename(&tmp, path)?;
        let dir = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        if let Ok(dir_file) = std::fs::File::open(dir) {
            let _ = dir_file.sync_all();
        }
        Ok(())
    }

    fn load_hnsw_file(&mut self, path: &Path) -> std::io::Result<()> {
        let bytes = std::fs::read(path)?;
        let bad = || std::io::Error::new(std::io::ErrorKind::InvalidData, "corrupt hnsw index");

        if bytes.len() < 64 {
            return Err(bad());
        }

        let payload_len = bytes.len() - 32;
        let expected_checksum = &bytes[payload_len..];
        let computed_checksum = blake3::hash(&bytes[..payload_len]);
        if computed_checksum.as_bytes() != expected_checksum {
            return Err(bad());
        }

        let payload = &bytes[..payload_len];
        if &payload[0..4] != HNSW_MAGIC {
            return Err(bad());
        }
        let version = u32::from_le_bytes(payload[4..8].try_into().unwrap());
        if version != 1 {
            return Err(bad());
        }

        let mut r = &payload[8..];
        let mut u32_buf = [0u8; 4];
        let mut u64_buf = [0u8; 8];

        r.read_exact(&mut u32_buf).map_err(|_| bad())?;
        let m = u32::from_le_bytes(u32_buf) as usize;
        r.read_exact(&mut u32_buf).map_err(|_| bad())?;
        let ef_c = u32::from_le_bytes(u32_buf) as usize;
        r.read_exact(&mut u64_buf).map_err(|_| bad())?;
        let ep = i64::from_le_bytes(u64_buf);
        r.read_exact(&mut u32_buf).map_err(|_| bad())?;
        let max_level = u32::from_le_bytes(u32_buf) as usize;
        r.read_exact(&mut u32_buf).map_err(|_| bad())?;
        let count = u32::from_le_bytes(u32_buf) as usize;

        let mut nodes = Vec::with_capacity(count);
        let mut id_to_node = HashMap::with_capacity(count);
        let mut deleted_count = 0;

        for node_idx in 0..count {
            let mut u16_buf = [0u8; 2];
            r.read_exact(&mut u16_buf).map_err(|_| bad())?;
            let id_len = u16::from_le_bytes(u16_buf) as usize;
            let mut id_bytes = vec![0u8; id_len];
            r.read_exact(&mut id_bytes).map_err(|_| bad())?;
            let id = String::from_utf8(id_bytes).map_err(|_| bad())?;

            r.read_exact(&mut u32_buf).map_err(|_| bad())?;
            let level = u32::from_le_bytes(u32_buf) as usize;

            let mut u8_buf = [0u8; 1];
            r.read_exact(&mut u8_buf).map_err(|_| bad())?;
            let deleted = u8_buf[0] != 0;

            let tombstone_vector = if deleted {
                deleted_count += 1;
                r.read_exact(&mut u8_buf).map_err(|_| bad())?;
                let has_tombstone = u8_buf[0] != 0;
                if has_tombstone {
                    r.read_exact(&mut u32_buf).map_err(|_| bad())?;
                    let dim = u32::from_le_bytes(u32_buf) as usize;
                    let mut f32_buf = [0u8; 4];
                    r.read_exact(&mut f32_buf).map_err(|_| bad())?;
                    let scale = f32::from_le_bytes(f32_buf);
                    let mut raw_data = vec![0u8; dim];
                    r.read_exact(&mut raw_data).map_err(|_| bad())?;
                    let v = raw_data.into_iter().map(|x| x as i8).collect();
                    Some((v, scale))
                } else {
                    None
                }
            } else {
                id_to_node.insert(id.clone(), node_idx as u32);
                None
            };

            let mut neighbors = Vec::with_capacity(level + 1);
            for _ in 0..=level {
                r.read_exact(&mut u32_buf).map_err(|_| bad())?;
                let n_count = u32::from_le_bytes(u32_buf) as usize;
                let mut layer_neighbors = Vec::with_capacity(n_count);
                for _ in 0..n_count {
                    r.read_exact(&mut u32_buf).map_err(|_| bad())?;
                    layer_neighbors.push(u32::from_le_bytes(u32_buf));
                }
                neighbors.push(layer_neighbors);
            }

            nodes.push(HnswNode {
                id,
                level,
                deleted,
                neighbors,
                tombstone_vector,
            });
        }

        if !r.is_empty() {
            return Err(bad());
        }

        self.m = m;
        self.m0 = 2 * m;
        self.ef_construction = ef_c;
        self.entry_point = if ep >= 0 { Some(ep as u32) } else { None };
        self.max_level = max_level;
        self.nodes = nodes;
        self.id_to_node = id_to_node;
        self.deleted_count = deleted_count;

        Ok(())
    }

    /// Loads an HNSW index written to `hnsw_path` wrapping `flat`.
    /// Missing or corrupt graph files are rebuilt from `flat` without returning an error.
    pub fn load(hnsw_path: &Path, flat: FlatIndex, ef_search: usize) -> std::io::Result<Self> {
        let mut index = Self::with_params(flat, DEFAULT_M, DEFAULT_EF_CONSTRUCTION, ef_search);
        index.hnsw_path = Some(hnsw_path.to_path_buf());

        if !hnsw_path.exists() {
            index.rebuild();
            return Ok(index);
        }

        match index.load_hnsw_file(hnsw_path) {
            Ok(()) => {
                let active_nodes = index.nodes.iter().filter(|n| !n.deleted).count();
                if active_nodes == index.flat.len() {
                    Ok(index)
                } else {
                    index.rebuild();
                    Ok(index)
                }
            }
            Err(_) => {
                index.rebuild();
                Ok(index)
            }
        }
    }
}

impl VectorIndex for HnswIndex {
    fn upsert(&mut self, id: &str, tag: &str, vector: &[f32]) {
        if self.id_to_node.contains_key(id) {
            self.remove(id);
        }
        self.flat.upsert(id, tag, vector);
        self.insert_graph_node(id, vector);
    }

    fn remove(&mut self, id: &str) -> bool {
        let Some(&node_id) = self.id_to_node.get(id) else {
            return false;
        };
        if self.nodes[node_id as usize].deleted {
            return false;
        }

        let (v, scale) = {
            let row = self.flat.row_of(id).expect("node must exist in flat");
            let (slice, scale) = self.flat.vector_at(row);
            (slice.to_vec(), scale)
        };

        self.nodes[node_id as usize].deleted = true;
        self.nodes[node_id as usize].tombstone_vector = Some((v, scale));
        self.id_to_node.remove(id);
        self.flat.remove(id);
        self.deleted_count += 1;

        if self.entry_point == Some(node_id) {
            let mut new_ep = None;
            for l in (0..=self.max_level).rev() {
                for (idx, n) in self.nodes.iter().enumerate() {
                    if !n.deleted && n.level >= l {
                        new_ep = Some(idx as u32);
                        self.max_level = l;
                        break;
                    }
                }
                if new_ep.is_some() {
                    break;
                }
            }
            self.entry_point = new_ep;
        }

        if !self.nodes.is_empty() && (self.deleted_count as f64 / self.nodes.len() as f64) > 0.20 {
            self.rebuild();
        }

        true
    }

    fn contains(&self, id: &str) -> bool {
        self.flat.contains(id)
    }

    fn search(&self, query: &[f32], k: usize) -> Vec<(String, f32)> {
        self.search_knn(query, k)
    }

    fn len(&self) -> usize {
        self.flat.len()
    }

    fn dim(&self) -> usize {
        self.flat.dim()
    }

    fn ids(&self) -> Box<dyn Iterator<Item = &str> + '_> {
        self.flat.ids()
    }

    fn tag(&self, id: &str) -> Option<&str> {
        self.flat.tag(id)
    }

    fn clear(&mut self) {
        let hnsw_path = self.hnsw_path.take();
        self.flat.clear();
        *self = Self::with_params(self.flat.clone(), self.m, self.ef_construction, self.ef_search);
        self.hnsw_path = hnsw_path;
    }

    fn persist(&self) -> std::io::Result<()> {
        self.flat.persist()?;
        match &self.hnsw_path {
            Some(path) => self.save_hnsw(path),
            None => match self.flat.path() {
                Some(p) => {
                    let hnsw_p = p.with_file_name("vectors.hnsw");
                    self.save_hnsw(&hnsw_p)
                }
                None => Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "no path configured for vector index persistence",
                )),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embed::normalize;

    fn rand_vecs(n: usize, dim: usize, seed: u64) -> Vec<Vec<f32>> {
        let mut s = seed;
        (0..n)
            .map(|_| {
                normalize(
                    (0..dim)
                        .map(|_| {
                            s = s
                                .wrapping_mul(6364136223846793005)
                                .wrapping_add(1442695040888963407);
                            ((s >> 33) as f32 / u32::MAX as f32) - 0.25
                        })
                        .collect(),
                )
            })
            .collect()
    }

    fn exact(vs: &[Vec<f32>], q: &[f32], k: usize) -> Vec<usize> {
        let mut s: Vec<(usize, f32)> = vs
            .iter()
            .enumerate()
            .map(|(i, v)| (i, v.iter().zip(q).map(|(a, b)| a * b).sum()))
            .collect();
        s.sort_by(|a, b| b.1.total_cmp(&a.1));
        s.into_iter().take(k).map(|(i, _)| i).collect()
    }

    /// The engine asks for a 40-candidate shortlist and rescores it with exact vectors;
    /// that is what must find the true top 10.
    fn shortlist_recall(idx: &HnswIndex, vs: &[Vec<f32>], queries: &[Vec<f32>]) -> f32 {
        let mut hits = 0;
        for q in queries {
            let truth: Vec<String> = exact(vs, q, 10).into_iter().map(|i| format!("c{i}")).collect();
            let mut short: Vec<(String, f32)> = idx
                .search(q, 40)
                .into_iter()
                .map(|(id, _)| {
                    let i: usize = id[1..].parse().unwrap();
                    (id, vs[i].iter().zip(q).map(|(a, b)| a * b).sum())
                })
                .collect();
            short.sort_by(|a, b| b.1.total_cmp(&a.1));
            hits += short.iter().take(10).filter(|(id, _)| truth.contains(id)).count();
        }
        hits as f32 / (queries.len() * 10) as f32
    }

    #[test]
    fn hnsw_search_recall_top10() {
        for (vs, queries) in [
            (rand_vecs(20000, 64, 42), rand_vecs(200, 64, 12345)),
            (
                clustered_vecs(20000, 64, 200, 42),
                clustered_vecs(200, 64, 200, 777),
            ),
        ] {
            let mut idx = HnswIndex::new();
            for (i, v) in vs.iter().enumerate() {
                idx.upsert(&format!("c{i}"), "t", v);
            }
            let recall = shortlist_recall(&idx, &vs, &queries);
            assert!(recall >= 0.95, "recall@10 was {recall}");
        }
    }

    #[test]
    fn persist_and_load_round_trip() {
        let vs = rand_vecs(100, 16, 7);
        let dir = tempfile::tempdir().unwrap();
        let bin_path = dir.path().join("vectors.bin");
        let hnsw_path = dir.path().join("vectors.hnsw");

        let mut idx = HnswIndex::new().with_path(&bin_path);
        for (i, v) in vs.iter().enumerate() {
            idx.upsert(&format!("chunk-{i}"), "tag", v);
        }
        assert_eq!(idx.len(), 100);
        idx.persist().unwrap();

        assert!(bin_path.exists());
        assert!(hnsw_path.exists());

        let flat = FlatIndex::load(&bin_path).unwrap();
        let loaded = HnswIndex::load(&hnsw_path, flat, 64).unwrap();
        assert_eq!(loaded.len(), 100);
        assert_eq!(loaded.dim(), 16);

        let q = &vs[0];
        let hits = loaded.search(q, 5);
        assert_eq!(hits[0].0, "chunk-0");
    }

    #[test]
    fn corrupt_vectors_hnsw_rebuilds() {
        let vs = rand_vecs(50, 16, 12);
        let dir = tempfile::tempdir().unwrap();
        let bin_path = dir.path().join("vectors.bin");
        let hnsw_path = dir.path().join("vectors.hnsw");

        let mut idx = HnswIndex::new().with_path(&bin_path);
        for (i, v) in vs.iter().enumerate() {
            idx.upsert(&format!("chunk-{i}"), "tag", v);
        }
        idx.persist().unwrap();

        // Missing hnsw file rebuilds from flat data
        std::fs::remove_file(&hnsw_path).unwrap();
        let flat = FlatIndex::load(&bin_path).unwrap();
        let rebuilt_from_missing = HnswIndex::load(&hnsw_path, flat, 64).unwrap();
        assert_eq!(rebuilt_from_missing.len(), 50);
        assert_eq!(rebuilt_from_missing.search(&vs[5], 1)[0].0, "chunk-5");

        // Corrupt hnsw file rebuilds from flat data
        std::fs::write(&hnsw_path, b"corrupted bytes garbage").unwrap();
        let flat = FlatIndex::load(&bin_path).unwrap();
        let rebuilt_from_corrupt = HnswIndex::load(&hnsw_path, flat, 64).unwrap();
        assert_eq!(rebuilt_from_corrupt.len(), 50);
        assert_eq!(rebuilt_from_corrupt.search(&vs[5], 1)[0].0, "chunk-5");
    }

    #[test]
    fn delete_then_rebuild() {
        let vs = rand_vecs(100, 16, 99);
        let mut idx = HnswIndex::new();
        for (i, v) in vs.iter().enumerate() {
            idx.upsert(&format!("c{i}"), "tag", v);
        }
        assert_eq!(idx.len(), 100);

        // Delete 10 nodes (10% <= 20%): marked deleted without full rebuild
        for i in 0..10 {
            assert!(idx.remove(&format!("c{i}")));
        }
        assert_eq!(idx.len(), 90);
        assert_eq!(idx.deleted_count, 10);
        for (i, v) in vs.iter().enumerate().take(10) {
            assert!(!idx.contains(&format!("c{i}")));
            assert!(idx.search(v, 1).iter().all(|(id, _)| id != &format!("c{i}")));
        }

        // Delete 15 more nodes (total 25 > 20%): triggers graph rebuild
        for i in 10..25 {
            assert!(idx.remove(&format!("c{i}")));
        }
        assert_eq!(idx.len(), 75);
        // The 21st deletion (over 20%) rebuilt the graph from the 79 live vectors; the
        // last 4 are tombstones in the new one.
        assert_eq!(idx.nodes.len(), 79);
        assert_eq!(idx.deleted_count, 4);

        for i in 25..100 {
            assert!(idx.contains(&format!("c{i}")));
        }
        assert_eq!(idx.search(&vs[30], 1)[0].0, "c30");
    }

    /// Clustered unit vectors: real embeddings concentrate around topics instead of filling
    /// the sphere uniformly.
    fn clustered_vecs(n: usize, dim: usize, clusters: usize, seed: u64) -> Vec<Vec<f32>> {
        let centres = rand_vecs(clusters, dim, seed ^ 0x9e37);
        let noise = rand_vecs(n, dim, seed);
        (0..n)
            .map(|i| {
                let c = &centres[i % clusters];
                normalize(c.iter().zip(&noise[i]).map(|(a, b)| a + 0.6 * b).collect())
            })
            .collect()
    }

    #[test]
    #[ignore]
    fn recall_report() {
        for (name, vs, queries) in [
            ("uniform", rand_vecs(20000, 64, 42), rand_vecs(200, 64, 12345)),
            (
                "clustered",
                clustered_vecs(20000, 64, 200, 42),
                clustered_vecs(200, 64, 200, 777),
            ),
        ] {
            let mut idx = HnswIndex::new();
            for (i, v) in vs.iter().enumerate() {
                idx.upsert(&format!("c{i}"), "t", v);
            }
            let t = std::time::Instant::now();
            for q in &queries {
                idx.flat().search(q, 40);
            }
            eprintln!(
                "{name} flat scan: {:?} per query",
                t.elapsed() / queries.len() as u32
            );
            for ef in [64, 128, 200] {
                idx.set_ef_search(ef);
                let (mut raw, mut rescored) = (0, 0);
                for q in &queries {
                    let truth: Vec<String> = exact(&vs, q, 10).into_iter().map(|i| format!("c{i}")).collect();
                    let got: Vec<String> = idx.search(q, 10).into_iter().map(|(id, _)| id).collect();
                    raw += got.iter().filter(|id| truth.contains(id)).count();
                    let mut short: Vec<(String, f32)> = idx
                        .search(q, 40)
                        .into_iter()
                        .map(|(id, _)| {
                            let i: usize = id[1..].parse().unwrap();
                            (id, vs[i].iter().zip(q).map(|(a, b)| a * b).sum())
                        })
                        .collect();
                    short.sort_by(|a, b| b.1.total_cmp(&a.1));
                    rescored += short.iter().take(10).filter(|(id, _)| truth.contains(id)).count();
                }
                let n = (queries.len() * 10) as f32;
                let t = std::time::Instant::now();
                for q in &queries {
                    idx.search(q, 40);
                }
                let per_query = t.elapsed() / queries.len() as u32;
                eprintln!(
                    "{name} ef={ef}: raw {:.3}, shortlist 40 rescored {:.3}, {per_query:?} per query",
                    raw as f32 / n,
                    rescored as f32 / n
                );
            }
        }
    }
}
