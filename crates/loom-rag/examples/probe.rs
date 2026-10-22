//! Indexes a project and runs queries in lexical and dense mode side by side.
//!
//!     cargo run --release --example probe -- <root> "<query>"...
//!
//! The index goes to `$TMPDIR/loom-rag-probe`; embeddings use the default gateway config.

use std::time::Instant;

use loom_rag::engine::{Config, Engine, Hit};

fn show(label: &str, hits: &[Hit]) {
    println!("  [{label}]");
    for h in hits {
        println!(
            "    {:.3} {}:{}-{} {}",
            h.score,
            h.path,
            h.start_line,
            h.end_line,
            h.scope.join(" › ")
        );
    }
}

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let root = args.next().unwrap_or_else(|| ".".into());
    let dir = std::env::temp_dir().join("loom-rag-probe");
    let mut engine = Engine::open(
        root.as_ref(),
        Config {
            index_dir: Some(dir),
            ..Config::default()
        },
    )?;
    println!("index: {:?}", engine.index(false)?);
    println!("re-index: {:?}", engine.index(false)?);
    println!("status: {:?}", engine.status()?);
    for q in args {
        println!("\n{q:?}");
        let t = Instant::now();
        let lex = engine.search_lexical(&q, 5)?;
        let lt = t.elapsed();
        let t = Instant::now();
        let dense = engine.search_dense(&q, 5)?;
        println!(
            "  lexical {lt:?}, dense {:?} (includes embedding the query)",
            t.elapsed()
        );
        show("lexical", &lex);
        show("dense", &dense);
    }
    Ok(())
}
