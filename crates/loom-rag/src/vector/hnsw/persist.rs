//! The on-disk format of the graph (`vectors.hnsw`): saving and loading it next to the
//! flat index it indexes.

use super::*;
use crate::vector::codec::{self, HashedWriter, Reader, write_atomically};

fn corrupt() -> std::io::Error {
    codec::corrupt("hnsw index")
}

/// The part of the file after the magic and the version, once the checksum, the magic and the
/// version are right.
fn verified_body(bytes: &[u8]) -> std::io::Result<&[u8]> {
    if bytes.len() < 64 {
        return Err(corrupt());
    }
    let payload_len = bytes.len() - 32;
    let payload = &bytes[..payload_len];
    if blake3::hash(payload).as_bytes() != &bytes[payload_len..] || &payload[0..4] != HNSW_MAGIC {
        return Err(corrupt());
    }
    let version = u32::from_le_bytes(payload[4..8].try_into().map_err(|_| corrupt())?);
    if version != 1 {
        return Err(corrupt());
    }
    Ok(&payload[8..])
}

fn write_node<W: Write>(w: &mut HashedWriter<W>, node: &HnswNode) -> std::io::Result<()> {
    w.put_str(&node.id)?;
    w.put_u32(node.level as u32)?;
    w.put(&[u8::from(node.deleted)])?;
    if node.deleted {
        write_tombstone(w, node.tombstone_vector.as_ref())?;
    }
    for neighbors in &node.neighbors[..=node.level] {
        w.put_u32(neighbors.len() as u32)?;
        for &n in neighbors {
            w.put(&n.to_le_bytes())?;
        }
    }
    Ok(())
}

fn write_tombstone<W: Write>(
    w: &mut HashedWriter<W>,
    tombstone: Option<&(Vec<i8>, f32)>,
) -> std::io::Result<()> {
    let Some((v, scale)) = tombstone else {
        return w.put(&[0u8]);
    };
    w.put(&[1u8])?;
    w.put_u32(v.len() as u32)?;
    w.put(&scale.to_le_bytes())?;
    let raw_bytes: Vec<u8> = v.iter().map(|&x| x as u8).collect();
    w.put(&raw_bytes)
}

fn read_node(r: &mut Reader) -> std::io::Result<HnswNode> {
    let id = r.string(true)?;
    let level = r.u32()? as usize;
    let deleted = r.u8()? != 0;
    let tombstone_vector = if deleted { read_tombstone(r)? } else { None };
    let mut neighbors = Vec::with_capacity(level + 1);
    for _ in 0..=level {
        let n_count = r.u32()? as usize;
        let mut layer = Vec::with_capacity(n_count);
        for _ in 0..n_count {
            layer.push(r.u32()?);
        }
        neighbors.push(layer);
    }
    Ok(HnswNode {
        id,
        level,
        deleted,
        neighbors,
        tombstone_vector,
    })
}

fn read_tombstone(r: &mut Reader) -> std::io::Result<Option<(Vec<i8>, f32)>> {
    if r.u8()? == 0 {
        return Ok(None);
    }
    let dim = r.u32()? as usize;
    let scale = r.f32()?;
    let v = r.take(dim)?.iter().map(|&x| x as i8).collect();
    Ok(Some((v, scale)))
}

impl HnswIndex {
    pub fn save_hnsw(&self, path: &Path) -> std::io::Result<()> {
        for node in &self.nodes {
            if node.id.len() > u16::MAX as usize {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("id length {} exceeds u16::MAX", node.id.len()),
                ));
            }
        }
        write_atomically(path, |w| self.write_graph(w))
    }

    fn write_graph<W: Write>(&self, w: &mut HashedWriter<W>) -> std::io::Result<()> {
        w.put(HNSW_MAGIC)?;
        w.put_u32(1)?;
        w.put_u32(self.m as u32)?;
        w.put_u32(self.ef_construction as u32)?;
        let ep = self.entry_point.map(|e| e as i64).unwrap_or(-1);
        w.put(&ep.to_le_bytes())?;
        w.put_u32(self.max_level as u32)?;
        w.put_u32(self.nodes.len() as u32)?;
        for node in &self.nodes {
            write_node(w, node)?;
        }
        Ok(())
    }

    pub(super) fn load_hnsw_file(&mut self, path: &Path) -> std::io::Result<()> {
        let bytes = std::fs::read(path)?;
        let mut r = Reader::new(verified_body(&bytes)?, "hnsw index");
        let m = r.u32()? as usize;
        let ef_c = r.u32()? as usize;
        let ep = i64::from_le_bytes(r.take(8)?.try_into().map_err(|_| corrupt())?);
        let max_level = r.u32()? as usize;
        let count = r.u32()? as usize;

        let mut nodes = Vec::with_capacity(count);
        let mut id_to_node = HashMap::with_capacity(count);
        let mut deleted_count = 0;
        for node_idx in 0..count {
            let node = read_node(&mut r)?;
            if node.deleted {
                deleted_count += 1;
            } else {
                id_to_node.insert(node.id.clone(), node_idx as u32);
            }
            nodes.push(node);
        }
        if !r.is_empty() {
            return Err(corrupt());
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

    /// Whether the loaded graph indexes exactly the ids of the flat index: the files are written
    /// one after the other, so a crash or a replaced vector can leave them out of step.
    fn matches_flat(&self) -> bool {
        let active = self.nodes.iter().filter(|n| !n.deleted).count();
        active == self.flat.len()
            && self
                .nodes
                .iter()
                .filter(|n| !n.deleted)
                .all(|n| self.flat.row_of(&n.id).is_some())
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
                if index.matches_flat() {
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
