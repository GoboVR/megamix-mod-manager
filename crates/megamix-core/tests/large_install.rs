//! Large-archive installs: files are streamed, never held in memory, and
//! zip64 (> 4 GiB) works. Runs in its own process so peak memory is ours.
//!
//! Default: 256 MiB. The 4.5 GiB version is opt-in:
//! `cargo test -p megamix-core --release --test large_install -- --ignored`

use std::fs;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::AtomicBool;

use megamix_core::install::{self, IfExists};
use megamix_core::mods::{Game, NewModPlacement};

fn disk_tempdir() -> tempfile::TempDir {
    let base = std::env::var_os("MEGAMIX_TEST_TMP")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| env!("CARGO_TARGET_TMPDIR").into());
    fs::create_dir_all(&base).unwrap();
    tempfile::tempdir_in(base).unwrap()
}

fn fake_game(dir: &Path) {
    fs::write(dir.join("DivaMegaMix.exe"), b"").unwrap();
    fs::write(
        dir.join("config.toml"),
        "enabled = true\nmods = \"mods\"\npriority = []\n",
    )
    .unwrap();
    fs::create_dir_all(dir.join("mods")).unwrap();
}

/// Write a zip with one big file, streaming (never building it in memory).
fn big_zip(path: &Path, size: u64) {
    let f = fs::File::create(path).unwrap();
    let mut z = zip::ZipWriter::new(std::io::BufWriter::new(f));
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .compression_level(Some(1))
        .large_file(size >= u32::MAX as u64);
    z.start_file("Song Pack/config.toml", opts).unwrap();
    z.write_all(b"name = \"Song Pack\"\n").unwrap();
    z.start_file("Song Pack/rom/big.bin", opts).unwrap();
    let chunk: Vec<u8> = (0..1u32 << 20).map(|i| (i % 251) as u8).collect();
    let mut left = size;
    while left > 0 {
        let n = left.min(chunk.len() as u64) as usize;
        z.write_all(&chunk[..n]).unwrap();
        left -= n as u64;
    }
    z.finish().unwrap();
}

/// Peak resident memory of this process in MiB (Linux).
fn peak_rss_mib() -> Option<u64> {
    let s = fs::read_to_string("/proc/self/status").ok()?;
    let line = s.lines().find(|l| l.starts_with("VmHWM:"))?;
    let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kb / 1024)
}

fn install_big(size: u64) {
    let t = disk_tempdir();
    fake_game(t.path());
    let archive = t.path().join("big.zip");
    big_zip(&archive, size);
    let before = peak_rss_mib();

    let mut game = Game::open(t.path()).unwrap();
    let mods_dir = game.mods_dir().unwrap();
    let plan = install::plan(&mods_dir, &archive).unwrap();
    assert_eq!(plan.mods[0].size, size + 19);
    let mut last = 0;
    let installed = install::install(
        &mods_dir,
        &plan,
        IfExists::Replace,
        &mut |s| {
            if let install::Step::Unpacking(p) = s {
                assert!(p.done >= last, "progress only goes forward");
                last = p.done;
            }
        },
        &AtomicBool::new(false),
    )
    .unwrap();
    install::register(&mut game, &installed, &archive, NewModPlacement::Top).unwrap();

    assert_eq!(
        fs::metadata(mods_dir.join("Song Pack/rom/big.bin"))
            .unwrap()
            .len(),
        size
    );
    assert_eq!(last, size + 19);
    if let (Some(b), Some(after)) = (before, peak_rss_mib()) {
        // Streaming: peak memory must not grow with the archive size.
        assert!(
            after < b.max(32) + 150,
            "peak memory {after} MiB (was {b} MiB) for a {} MiB file",
            size >> 20
        );
    }
}

#[test]
fn installs_256_mib_mod_streaming() {
    install_big(256 << 20);
}

#[test]
#[ignore = "writes ~9 GB; run with --release --ignored"]
fn installs_4_5_gib_mod_zip64() {
    install_big((4 << 30) + (512 << 20));
}
