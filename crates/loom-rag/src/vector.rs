//! Exact vector search over int8-quantized embeddings (`docs/specs/retrieval.md` § Dense index).
//!
//! Each unit vector is stored as `i8` components plus one `f32` scale (`max|x| / 127`), a 4×
//! memory saving over `f32`: 50k chunks of 768 dimensions take about 38 MB. The query is
//! quantized the same way, so scoring is an integer dot product that the compiler
//! vectorizes; rows are scanned in parallel.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use rayon::prelude::*;

#[cfg(feature = "hnsw")]
pub mod hnsw;
#[cfg(feature = "hnsw")]
pub use hnsw::HnswIndex;

/// A nearest-neighbour index keyed by chunk id.
pub trait VectorIndex: Send + Sync {
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
    fn dim(&self) -> usize;
    fn ids(&self) -> Box<dyn Iterator<Item = &str> + '_>;
    fn tag(&self, id: &str) -> Option<&str>;
    fn clear(&mut self);
    fn persist(&self) -> std::io::Result<()>;
}

#[derive(Debug, Clone, Default)]
pub struct FlatIndex {
    dim: usize,
    ids: Vec<String>,
    tags: Vec<String>,
    scales: Vec<f32>,
    data: Vec<i8>,
    pos: HashMap<String, usize>,
    path: Option<PathBuf>,
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

    pub fn set_path(&mut self, path: impl Into<PathBuf>) {
        self.path = Some(path.into());
    }

    pub fn with_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.path = Some(path.into());
        self
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn dim(&self) -> usize {
        self.dim
    }

    pub fn ids(&self) -> Box<dyn Iterator<Item = &str> + '_> {
        Box::new(self.ids.iter().map(String::as_str))
    }

    /// The tag stored with `id`.
    pub fn tag(&self, id: &str) -> Option<&str> {
        self.pos.get(id).map(|&row| self.tags[row].as_str())
    }

    pub fn row_of(&self, id: &str) -> Option<usize> {
        self.pos.get(id).copied()
    }

    pub fn vector_at(&self, row: usize) -> (&[i8], f32) {
        let start = row * self.dim;
        (&self.data[start..start + self.dim], self.scales[row])
    }

    pub fn vector_f32(&self, row: usize) -> Vec<f32> {
        let (data, scale) = self.vector_at(row);
        data.iter().map(|&x| x as f32 * scale).collect()
    }

    pub fn id_at(&self, row: usize) -> &str {
        &self.ids[row]
    }

    pub fn tag_at(&self, row: usize) -> &str {
        &self.tags[row]
    }

    pub fn clear(&mut self) {
        let path = self.path.take();
        *self = Self::default();
        self.path = path;
    }

    pub fn persist(&self) -> std::io::Result<()> {
        match &self.path {
            Some(path) => self.save(path),
            None => Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "no path configured for vector index persistence",
            )),
        }
    }

    /// Writes the index atomically (temporary file, then rename).
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        for (row, id) in self.ids.iter().enumerate() {
            check_u16_len("id", id)?;
            check_u16_len("tag", &self.tags[row])?;
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
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let mut idx = Self::new();
                idx.path = Some(path.to_path_buf());
                return Ok(idx);
            }
            Err(e) => return Err(e),
        };
        let bad = || std::io::Error::new(std::io::ErrorKind::InvalidData, "corrupt vector index");
        if bytes.len() < 4 {
            return Err(bad());
        }
        let magic = &bytes[0..4];
        if magic == MAGIC_V1 {
            let mut idx = Self::new();
            idx.path = Some(path.to_path_buf());
            return Ok(idx);
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
            let id = read_prefixed_string(&mut r, is_v3).ok_or_else(bad)?;

            let tag = read_prefixed_string(&mut r, is_v3).ok_or_else(bad)?;

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
        idx.path = Some(path.to_path_buf());
        Ok(idx)
    }
}

/// Quantizes a vector to `i8` with a symmetric per-vector scale.
pub(crate) fn quantize(v: &[f32]) -> (Vec<i8>, f32) {
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
pub(crate) fn dot_i8(a: &[i8], b: &[i8]) -> i32 {
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

    fn dim(&self) -> usize {
        self.dim
    }

    fn ids(&self) -> Box<dyn Iterator<Item = &str> + '_> {
        Box::new(self.ids.iter().map(String::as_str))
    }

    fn tag(&self, id: &str) -> Option<&str> {
        self.pos.get(id).map(|&row| self.tags[row].as_str())
    }

    fn clear(&mut self) {
        FlatIndex::clear(self);
    }

    fn persist(&self) -> std::io::Result<()> {
        FlatIndex::persist(self)
    }
}

#[cfg(test)]
mod tests;

/// Fails when `value` is too long for the u16 length prefix of the v3 format.
fn check_u16_len(what: &str, value: &str) -> std::io::Result<()> {
    if value.len() > u16::MAX as usize {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{what} length {} exceeds u16::MAX (65535)", value.len()),
        ));
    }
    Ok(())
}

/// Reads a length-prefixed UTF-8 string: a u16 length in v3 files, a u8 length before.
fn read_prefixed_string(r: &mut &[u8], is_v3: bool) -> Option<String> {
    let len = if is_v3 {
        let mut b = [0u8; 2];
        r.read_exact(&mut b).ok()?;
        u16::from_le_bytes(b) as usize
    } else {
        let mut b = [0u8; 1];
        r.read_exact(&mut b).ok()?;
        b[0] as usize
    };
    let mut bytes = vec![0u8; len];
    r.read_exact(&mut bytes).ok()?;
    String::from_utf8(bytes).ok()
}
