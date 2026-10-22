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
    back.persist().unwrap();
    assert_eq!(FlatIndex::load(&p).unwrap().len(), 4);
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
