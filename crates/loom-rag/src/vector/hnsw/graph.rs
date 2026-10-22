//! Graph construction and search: level assignment, greedy layer search, the neighbour
//! selection heuristic, insertion, rebuild and k-NN search over int8 vectors.

use super::*;

impl HnswIndex {
    pub(super) fn random_level(&mut self) -> usize {
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

    pub(super) fn node_vector_data(&self, node_id: u32) -> (&[i8], f32) {
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

    pub(super) fn dist_to_q(&self, q_i8: &[i8], q_scale: f32, node_id: u32) -> f32 {
        let (v, v_scale) = self.node_vector_data(node_id);
        let sim = dot_i8(q_i8, v) as f32 * q_scale * v_scale;
        1.0 - sim
    }

    pub(super) fn dist_nodes(&self, a: u32, b: u32) -> f32 {
        let (va, sa) = self.node_vector_data(a);
        let (vb, sb) = self.node_vector_data(b);
        let sim = dot_i8(va, vb) as f32 * sa * sb;
        1.0 - sim
    }

    pub(super) fn search_layer(
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

    pub(super) fn select_neighbors_heuristic(
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

    pub(super) fn insert_graph_node(&mut self, id: &str, vector: &[f32]) {
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
}
