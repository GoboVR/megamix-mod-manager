//! Print the shortcuts in a shortcuts.vdf (read-only).
fn main() {
    let p = std::env::args().nth(1).expect("file");
    let m = megamix_core::binvdf::parse(&std::fs::read(p).unwrap()).unwrap();
    for (k, v) in &m.map("shortcuts").unwrap().0 {
        if let megamix_core::binvdf::Value::Map(e) = v {
            println!(
                "{}: {:?} exe={:?} appid={:?} launch={:?}",
                String::from_utf8_lossy(k),
                e.str("AppName"),
                e.str("Exe"),
                e.int("appid").map(|i| i as u32),
                e.str("LaunchOptions")
            );
        }
    }
}
