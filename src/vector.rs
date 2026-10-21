//! Exact vector search over int8-quantized embeddings (`docs/specs/retrieval.md` § Dense index).
//!
//! Each unit vector is stored as `i8` components plus one `f32` scale (`max|x| / 127`), a 4×
//! memory saving over `f32`: 50k chunks of 768 dimensions take about 38 MB. The query is
//! quantized the same way, so scoring is an integer dot product that the compiler
//! vectorizes; rows are scanned in parallel.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::Path;

use rayon::prelude::*;

/// A nearest-neighbour index keyed by chunk id.
pub trait VectorIndex {
    /// Inserts or replaces `id`. `tag` is an opaque label kept with the row (the engine
    /// stores the chunk's content hash, to find its exact vector for rescoring).
    fn upsert(&mut self, id: &str, tag: &str, vector: &[f32]);
    fn remove(&mut self, id: &str) -> bool;
    fn contains(&self, id: &str) -> bool;
    /// The `k` most similar ids by cosine similarity, best first.
    fn search(&self, query: &[f32], k: usize) -> Vec<(String, f32)>;
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[derive(Debug, Clone, Default)]
pub struct FlatIndex {
    dim: usize,
    ids: Vec<String>,
    tags: Vec<String>,
    scales: Vec<f32>,
    data: Vec<i8>,
    pos: HashMap<String, usize>,
}

const MAGIC: &[u8; 4] = b"DRV2";
/// Earlier format without tags: loaded as empty, so vectors are rebuilt from the cache.
const MAGIC_V1: &[u8; 4] = b"DRV1";
/// Rows per parallel work unit.
const BLOCK: usize = 1024;

impl FlatIndex {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn dim(&self) -> usize {
        self.dim
    }

    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.ids.iter().map(String::as_str)
    }

    /// The tag stored with `id`.
    pub fn tag(&self, id: &str) -> Option<&str> {
        self.pos.get(id).map(|&row| self.tags[row].as_str())
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// Writes the index atomically (temporary file, then rename).
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let tmp = path.with_extension("tmp");
        {
            let mut w = std::io::BufWriter::new(std::fs::File::create(&tmp)?);
            w.write_all(MAGIC)?;
            w.write_all(&(self.dim as u32).to_le_bytes())?;
            w.write_all(&(self.ids.len() as u32).to_le_bytes())?;
            for (row, id) in self.ids.iter().enumerate() {
                w.write_all(&[id.len() as u8])?;
                w.write_all(id.as_bytes())?;
                w.write_all(&[self.tags[row].len() as u8])?;
                w.write_all(self.tags[row].as_bytes())?;
                w.write_all(&self.scales[row].to_le_bytes())?;
                let v = &self.data[row * self.dim..(row + 1) * self.dim];
                w.write_all(&v.iter().map(|&x| x as u8).collect::<Vec<_>>())?;
            }
            w.flush()?;
        }
        std::fs::rename(tmp, path)
    }

    /// Loads an index written by [`FlatIndex::save`]; a missing file gives an empty index.
    pub fn load(path: &Path) -> std::io::Result<Self> {
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::new()),
            Err(e) => return Err(e),
        };
        let bad = || std::io::Error::new(std::io::ErrorKind::InvalidData, "corrupt vector index");
        let mut r = bytes.as_slice();
        let mut magic = [0u8; 4];
        r.read_exact(&mut magic)?;
        if &magic == MAGIC_V1 {
            return Ok(Self::new());
        }
        if &magic != MAGIC {
            return Err(bad());
        }
        let mut u32b = [0u8; 4];
        r.read_exact(&mut u32b)?;
        let dim = u32::from_le_bytes(u32b) as usize;
        r.read_exact(&mut u32b)?;
        let count = u32::from_le_bytes(u32b) as usize;
        let mut idx = Self {
            dim,
            ..Self::default()
        };
        idx.data.reserve(dim * count);
        for row in 0..count {
            let mut len = [0u8; 1];
            r.read_exact(&mut len)?;
            let mut id = vec![0u8; len[0] as usize];
            r.read_exact(&mut id)?;
            let id = String::from_utf8(id).map_err(|_| bad())?;
            r.read_exact(&mut len)?;
            let mut tag = vec![0u8; len[0] as usize];
            r.read_exact(&mut tag)?;
            idx.tags.push(String::from_utf8(tag).map_err(|_| bad())?);
            let mut f = [0u8; 4];
            r.read_exact(&mut f)?;
            let mut v = vec![0u8; dim];
            r.read_exact(&mut v)?;
            idx.pos.insert(id.clone(), row);
            idx.ids.push(id);
            idx.scales.push(f32::from_le_bytes(f));
            idx.data.extend(v.into_iter().map(|x| x as i8));
        }
        Ok(idx)
    }
}

/// Quantizes a vector to `i8` with a symmetric per-vector scale.
fn quantize(v: &[f32]) -> (Vec<i8>, f32) {
    let max = v.iter().fold(0f32, |m, x| m.max(x.abs()));
    if max == 0.0 {
        return (vec![0; v.len()], 0.0);
    }
    let scale = max / 127.0;
    (
        v.iter()
            .map(|x| (x / scale).round().clamp(-127.0, 127.0) as i8)
            .collect(),
        scale,
    )
}

#[inline]
fn dot_i8(a: &[i8], b: &[i8]) -> i32 {
    a.iter().zip(b).map(|(&x, &y)| x as i32 * y as i32).sum()
}

impl VectorIndex for FlatIndex {
    fn upsert(&mut self, id: &str, tag: &str, vector: &[f32]) {
        if self.dim == 0 {
            self.dim = vector.len();
        }
        assert_eq!(vector.len(), self.dim, "vector dimension mismatch");
        let (q, scale) = quantize(vector);
        match self.pos.get(id) {
            Some(&row) => {
                self.data[row * self.dim..(row + 1) * self.dim].copy_from_slice(&q);
                self.scales[row] = scale;
                self.tags[row] = tag.to_string();
            }
            None => {
                self.pos.insert(id.to_string(), self.ids.len());
                self.ids.push(id.to_string());
                self.tags.push(tag.to_string());
                self.scales.push(scale);
                self.data.extend(q);
            }
        }
    }

    fn remove(&mut self, id: &str) -> bool {
        let Some(row) = self.pos.remove(id) else {
            return false;
        };
        let last = self.ids.len() - 1;
        if row != last {
            let (head, tail) = self.data.split_at_mut(last * self.dim);
            head[row * self.dim..(row + 1) * self.dim].copy_from_slice(&tail[..self.dim]);
            self.ids.swap(row, last);
            self.tags.swap(row, last);
            self.scales.swap(row, last);
            self.pos.insert(self.ids[row].clone(), row);
        }
        self.ids.pop();
        self.tags.pop();
        self.scales.pop();
        self.data.truncate(last * self.dim);
        true
    }

    fn contains(&self, id: &str) -> bool {
        self.pos.contains_key(id)
    }

    fn search(&self, query: &[f32], k: usize) -> Vec<(String, f32)> {
        if self.ids.is_empty() || k == 0 || query.len() != self.dim {
            return Vec::new();
        }
        let (q, qscale) = quantize(query);
        let dim = self.dim;
        let mut scored: Vec<(usize, f32)> = self
            .data
            .par_chunks(dim * BLOCK)
            .enumerate()
            .flat_map_iter(|(block, rows)| {
                let q = &q;
                rows.chunks_exact(dim).enumerate().map(move |(i, v)| {
                    let row = block * BLOCK + i;
                    (row, dot_i8(q, v) as f32 * qscale * self.scales[row])
                })
            })
            .collect();
        let k = k.min(scored.len());
        if k < scored.len() {
            scored.select_nth_unstable_by(k, |a, b| b.1.total_cmp(&a.1));
            scored.truncate(k);
        }
        scored.sort_by(|a, b| b.1.total_cmp(&a.1));
        scored
            .into_iter()
            .map(|(row, s)| (self.ids[row].clone(), s))
            .collect()
    }

    fn len(&self) -> usize {
        self.ids.len()
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

    #[test]
    fn quantized_search_matches_exact_top10() {
        let vs = rand_vecs(3000, 128, 7);
        let mut idx = FlatIndex::new();
        for (i, v) in vs.iter().enumerate() {
            idx.upsert(&format!("c{i}"), "t", v);
        }
        let queries = rand_vecs(50, 128, 99);
        let mut hits = 0;
        for q in &queries {
            let truth: Vec<String> = exact(&vs, q, 10).into_iter().map(|i| format!("c{i}")).collect();
            let got: Vec<String> = idx.search(q, 10).into_iter().map(|(id, _)| id).collect();
            hits += got.iter().filter(|id| truth.contains(id)).count();
        }
        let recall = hits as f32 / (queries.len() * 10) as f32;
        assert!(recall >= 0.95, "recall@10 {recall}");
    }

    #[test]
    fn upsert_remove_and_persist() {
        let vs = rand_vecs(5, 16, 1);
        let mut idx = FlatIndex::new();
        for (i, v) in vs.iter().enumerate() {
            idx.upsert(&format!("c{i}"), "t", v);
        }
        assert_eq!(idx.search(&vs[3], 1)[0].0, "c3");
        assert!(idx.remove("c1"));
        assert!(!idx.remove("c1"));
        assert_eq!(idx.len(), 4);
        assert_eq!(idx.search(&vs[4], 1)[0].0, "c4");
        idx.upsert("c4", "t4", &vs[0]);
        assert_eq!(idx.len(), 4);

        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("v.bin");
        idx.save(&p).unwrap();
        let back = FlatIndex::load(&p).unwrap();
        assert_eq!(back.len(), 4);
        assert!(back.contains("c4") && !back.contains("c1"));
        assert_eq!(back.tag("c4"), Some("t4"));
        assert_eq!(back.tag("c0"), Some("t"));
        assert_eq!(back.search(&vs[3], 1)[0].0, "c3");
        assert_eq!(FlatIndex::load(&dir.path().join("missing")).unwrap().len(), 0);
    }
}
