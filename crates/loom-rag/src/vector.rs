//! Exact vector search over int8-quantized embeddings (`docs/specs/retrieval.md` § Dense index).
//!
//! Each unit vector is stored as `i8` components plus one `f32` scale (`max|x| / 127`), a 4×
//! memory saving over `f32`: 50k chunks of 768 dimensions take about 38 MB. The query is
//! quantized the same way, so scoring is an integer dot product that the compiler
//! vectorizes; rows are scanned in parallel.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use rayon::prelude::*;

use self::codec::{HashedWriter, Reader, write_atomically};

mod codec;
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
        write_atomically(path, |w| self.write_rows(w))
    }

    fn write_rows<W: Write>(&self, w: &mut HashedWriter<W>) -> std::io::Result<()> {
        w.put(MAGIC)?;
        w.put_u32(self.dim as u32)?;
        w.put_u32(self.ids.len() as u32)?;
        let mut row_buf = Vec::with_capacity(self.dim);
        for row in 0..self.ids.len() {
            w.put_str(&self.ids[row])?;
            w.put_str(&self.tags[row])?;
            w.put(&self.scales[row].to_le_bytes())?;
            row_buf.clear();
            row_buf.extend(
                self.data[row * self.dim..(row + 1) * self.dim]
                    .iter()
                    .map(|&x| x as u8),
            );
            w.put(&row_buf)?;
        }
        Ok(())
    }

    /// Loads an index written by [`FlatIndex::save`]; a missing file gives an empty index.
    pub fn load(path: &Path) -> std::io::Result<Self> {
        let mut idx = match std::fs::read(path) {
            Ok(bytes) => Self::decode(&bytes)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::new(),
            Err(e) => return Err(e),
        };
        idx.path = Some(path.to_path_buf());
        Ok(idx)
    }

    fn decode(bytes: &[u8]) -> std::io::Result<Self> {
        let Some((v3, payload)) = flat_payload(bytes)? else {
            return Ok(Self::new());
        };
        let mut r = Reader::new(payload, "vector index");
        let dim = r.u32()? as usize;
        let count = r.u32()? as usize;
        let mut idx = Self {
            dim,
            ..Self::default()
        };
        idx.data.reserve(dim * count);
        idx.ids.reserve(count);
        idx.tags.reserve(count);
        idx.scales.reserve(count);
        for row in 0..count {
            idx.read_row(&mut r, v3, row)?;
        }
        if !r.is_empty() {
            return Err(r.corrupt());
        }
        Ok(idx)
    }

    /// Reads row `row`: id, tag, scale and the int8 components.
    fn read_row(&mut self, r: &mut Reader, v3: bool, row: usize) -> std::io::Result<()> {
        let id = r.string(v3)?;
        let tag = r.string(v3)?;
        let scale = r.f32()?;
        let components = r.take(self.dim)?;
        if self.pos.insert(id.clone(), row).is_some() {
            return Err(r.corrupt());
        }
        self.ids.push(id);
        self.tags.push(tag);
        self.scales.push(scale);
        self.data.extend(components.iter().map(|&x| x as i8));
        Ok(())
    }
}

/// The rows of a flat index file and whether it is a v3 file (u16 string lengths, a checksum
/// at the end); `None` for the v1 format, which held no rows worth keeping.
fn flat_payload(bytes: &[u8]) -> std::io::Result<Option<(bool, &[u8])>> {
    let bad = || codec::corrupt("vector index");
    if bytes.len() < 4 {
        return Err(bad());
    }
    match &bytes[0..4] {
        m if m == MAGIC_V1 => Ok(None),
        m if m == MAGIC_V2 => Ok(Some((false, &bytes[4..]))),
        m if m == MAGIC => {
            if bytes.len() < 44 {
                return Err(bad());
            }
            let payload_len = bytes.len() - 32;
            if blake3::hash(&bytes[..payload_len]).as_bytes() != &bytes[payload_len..] {
                return Err(bad());
            }
            Ok(Some((true, &bytes[4..payload_len])))
        }
        _ => Err(bad()),
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
