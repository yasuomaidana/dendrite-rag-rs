//! Approximate nearest-neighbour index using Hierarchical Navigable Small World graphs.
//!
//! Wraps a [`FlatIndex`] to keep int8 vector storage in `vectors.bin` while maintaining
//! an in-memory graph index persisted to `vectors.hnsw`.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use crate::vector::{FlatIndex, VectorIndex, dot_i8, quantize};

mod graph;
mod persist;

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
mod tests;
