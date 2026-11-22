//! Check that real shortcuts.vdf files parse and write back unchanged:
//! `cargo run -p megamix-core --example vdf_roundtrip -- <files>` (read-only).
fn main() {
    for p in std::env::args().skip(1) {
        let b = std::fs::read(&p).unwrap();
        match megamix_core::binvdf::parse(&b) {
            Ok(m) => {
                let n = m.map("shortcuts").map(|l| l.0.len()).unwrap_or(0);
                let same = megamix_core::binvdf::write(&m) == b;
                println!("{p}: {n} shortcuts, round-trip identical: {same}");
            }
            Err(e) => println!("{p}: {e}"),
        }
    }
}
