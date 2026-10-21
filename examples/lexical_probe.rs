//! Indexes a project and runs lexical queries.
//!
//!     cargo run --release --example lexical_probe -- <root> "<query>"...

use std::time::Instant;

use dendrite_rag::engine::{Config, Engine};

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let root = args.next().unwrap_or_else(|| ".".into());
    let dir = std::env::temp_dir().join("dendrite-rag-probe");
    let mut engine = Engine::open(
        root.as_ref(),
        Config {
            index_dir: Some(dir),
            ..Config::default()
        },
    )?;
    println!("index: {:?}", engine.index(false)?);
    println!("re-index: {:?}", engine.index(false)?);
    for q in args {
        let t = Instant::now();
        let hits = engine.search_lexical(&q, 5)?;
        println!("\n{q:?} ({:?})", t.elapsed());
        for h in hits {
            println!(
                "  {:.2} {}:{}-{} {}",
                h.score,
                h.path,
                h.start_line,
                h.end_line,
                h.scope.join(" › ")
            );
        }
    }
    Ok(())
}
