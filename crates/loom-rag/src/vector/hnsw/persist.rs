//! The on-disk format of the graph (`vectors.hnsw`): saving and loading it next to the
//! flat index it indexes.

use super::*;

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

    pub(super) fn load_hnsw_file(&mut self, path: &Path) -> std::io::Result<()> {
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
