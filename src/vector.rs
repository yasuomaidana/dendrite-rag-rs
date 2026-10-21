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

const MAGIC: &[u8; 4] = b"DRV3";
const MAGIC_V2: &[u8; 4] = b"DRV2";
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
        for (row, id) in self.ids.iter().enumerate() {
            if id.len() > u16::MAX as usize {
                let len = id.len();
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("id length {len} exceeds u16::MAX (65535)"),
                ));
            }
            let tag = &self.tags[row];
            if tag.len() > u16::MAX as usize {
                let len = tag.len();
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("tag length {len} exceeds u16::MAX (65535)"),
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
                write_hashed(MAGIC)?;
                write_hashed(&(self.dim as u32).to_le_bytes())?;
                write_hashed(&(self.ids.len() as u32).to_le_bytes())?;
                let mut row_buf = Vec::with_capacity(self.dim);
                for (row, id) in self.ids.iter().enumerate() {
                    write_hashed(&(id.len() as u16).to_le_bytes())?;
                    write_hashed(id.as_bytes())?;
                    let tag = &self.tags[row];
                    write_hashed(&(tag.len() as u16).to_le_bytes())?;
                    write_hashed(tag.as_bytes())?;
                    write_hashed(&self.scales[row].to_le_bytes())?;
                    let v = &self.data[row * self.dim..(row + 1) * self.dim];
                    row_buf.clear();
                    row_buf.extend(v.iter().map(|&x| x as u8));
                    write_hashed(&row_buf)?;
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

    /// Loads an index written by [`FlatIndex::save`]; a missing file gives an empty index.
    pub fn load(path: &Path) -> std::io::Result<Self> {
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::new()),
            Err(e) => return Err(e),
        };
        let bad = || std::io::Error::new(std::io::ErrorKind::InvalidData, "corrupt vector index");
        if bytes.len() < 4 {
            return Err(bad());
        }
        let magic = &bytes[0..4];
        if magic == MAGIC_V1 {
            return Ok(Self::new());
        }
        let is_v3 = if magic == MAGIC {
            true
        } else if magic == MAGIC_V2 {
            false
        } else {
            return Err(bad());
        };
        if is_v3 {
            if bytes.len() < 44 {
                return Err(bad());
            }
            let payload_len = bytes.len() - 32;
            let expected = &bytes[payload_len..];
            let hash = blake3::hash(&bytes[..payload_len]);
            if hash.as_bytes() != expected {
                return Err(bad());
            }
        }
        let payload = if is_v3 {
            &bytes[4..bytes.len() - 32]
        } else {
            &bytes[4..]
        };
        let mut r = payload;
        let mut u32b = [0u8; 4];
        r.read_exact(&mut u32b).map_err(|_| bad())?;
        let dim = u32::from_le_bytes(u32b) as usize;
        r.read_exact(&mut u32b).map_err(|_| bad())?;
        let count = u32::from_le_bytes(u32b) as usize;
        let mut idx = Self {
            dim,
            ..Self::default()
        };
        idx.data.reserve(dim * count);
        idx.ids.reserve(count);
        idx.tags.reserve(count);
        idx.scales.reserve(count);
        for row in 0..count {
            let id_len = if is_v3 {
                let mut b = [0u8; 2];
                r.read_exact(&mut b).map_err(|_| bad())?;
                u16::from_le_bytes(b) as usize
            } else {
                let mut b = [0u8; 1];
                r.read_exact(&mut b).map_err(|_| bad())?;
                b[0] as usize
            };
            let mut id = vec![0u8; id_len];
            r.read_exact(&mut id).map_err(|_| bad())?;
            let id = String::from_utf8(id).map_err(|_| bad())?;

            let tag_len = if is_v3 {
                let mut b = [0u8; 2];
                r.read_exact(&mut b).map_err(|_| bad())?;
                u16::from_le_bytes(b) as usize
            } else {
                let mut b = [0u8; 1];
                r.read_exact(&mut b).map_err(|_| bad())?;
                b[0] as usize
            };
            let mut tag = vec![0u8; tag_len];
            r.read_exact(&mut tag).map_err(|_| bad())?;
            let tag = String::from_utf8(tag).map_err(|_| bad())?;

            let mut f = [0u8; 4];
            r.read_exact(&mut f).map_err(|_| bad())?;
            let mut v = vec![0u8; dim];
            r.read_exact(&mut v).map_err(|_| bad())?;

            if idx.pos.insert(id.clone(), row).is_some() {
                return Err(bad());
            }
            idx.ids.push(id);
            idx.tags.push(tag);
            idx.scales.push(f32::from_le_bytes(f));
            idx.data.extend(v.into_iter().map(|x| x as i8));
        }
        if !r.is_empty() {
            return Err(bad());
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

    #[test]
    fn v3_format_300_byte_ids_and_checksum() {
        let vs = rand_vecs(3, 16, 42);
        let mut idx = FlatIndex::new();
        let long_id = "a".repeat(300);
        let long_tag = "tag-".repeat(75);
        idx.upsert(&long_id, &long_tag, &vs[0]);
        idx.upsert("short", "tag", &vs[1]);

        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("v3.bin");
        idx.save(&p).unwrap();

        let bytes = std::fs::read(&p).unwrap();
        assert!(bytes.starts_with(b"DRV3"));

        let back = FlatIndex::load(&p).unwrap();
        assert_eq!(back.len(), 2);
        assert!(back.contains(&long_id));
        assert!(back.contains("short"));
        assert_eq!(back.tag(&long_id), Some(long_tag.as_str()));
        assert_eq!(back.tag("short"), Some("tag"));

        let mut corrupt = bytes.clone();
        let last = corrupt.len() - 1;
        corrupt[last] ^= 0x01;
        let corrupt_p = dir.path().join("corrupt.bin");
        std::fs::write(&corrupt_p, &corrupt).unwrap();
        assert!(FlatIndex::load(&corrupt_p).is_err());

        let trunc_p = dir.path().join("trunc.bin");
        std::fs::write(&trunc_p, &bytes[..bytes.len() - 10]).unwrap();
        assert!(FlatIndex::load(&trunc_p).is_err());

        let mut huge_idx = FlatIndex::new();
        let huge_id = "x".repeat(65536);
        huge_idx.upsert(&huge_id, "t", &vs[0]);
        let huge_p = dir.path().join("huge.bin");
        assert!(huge_idx.save(&huge_p).is_err());
    }

    #[test]
    fn loads_legacy_drv2_format() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("v2.bin");
        let dim: u32 = 4;
        let count: u32 = 1;
        let mut buf = Vec::new();
        buf.extend_from_slice(b"DRV2");
        buf.extend_from_slice(&dim.to_le_bytes());
        buf.extend_from_slice(&count.to_le_bytes());
        let id = b"chunk-1";
        buf.push(id.len() as u8);
        buf.extend_from_slice(id);
        let tag = b"tag-1";
        buf.push(tag.len() as u8);
        buf.extend_from_slice(tag);
        buf.extend_from_slice(&1.5f32.to_le_bytes());
        buf.extend_from_slice(&[10u8, 20, 30, 40]);
        std::fs::write(&p, &buf).unwrap();

        let loaded = FlatIndex::load(&p).unwrap();
        assert_eq!(loaded.len(), 1);
        assert!(loaded.contains("chunk-1"));
        assert_eq!(loaded.tag("chunk-1"), Some("tag-1"));
    }
}
