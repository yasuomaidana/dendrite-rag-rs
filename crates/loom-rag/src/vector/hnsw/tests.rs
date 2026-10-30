use super::*;
use crate::embed::normalize;
use crate::testutil::{exact, rand_vecs};

/// The shortlist an index returns for `q` (`size` candidates), rescored with the exact vectors and best first.
fn rescored_shortlist(idx: &HnswIndex, vs: &[Vec<f32>], q: &[f32], size: usize) -> Vec<(String, f32)> {
    let mut short: Vec<(String, f32)> = idx
        .search(q, size)
        .into_iter()
        .map(|(id, _)| {
            let i: usize = id[1..].parse().unwrap();
            (id, vs[i].iter().zip(q).map(|(a, b)| a * b).sum())
        })
        .collect();
    short.sort_by(|a, b| b.1.total_cmp(&a.1));
    short
}

/// The engine asks for a 40-candidate shortlist and rescores it with exact vectors;
/// that is what must find the true top 10.
fn shortlist_recall(idx: &HnswIndex, vs: &[Vec<f32>], queries: &[Vec<f32>]) -> f32 {
    let mut hits = 0;
    for q in queries {
        let truth: Vec<String> = exact(vs, q, 10).into_iter().map(|i| format!("c{i}")).collect();
        let short = rescored_shortlist(idx, vs, q, 40);
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
                let short = rescored_shortlist(&idx, &vs, q, 40);
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
