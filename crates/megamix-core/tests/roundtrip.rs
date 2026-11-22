//! Round-trip tests against real configs copied from the Thor.

use std::fs;
use std::path::PathBuf;

use megamix_core::config::{LineEnding, LoaderConfig, ModConfig, TomlFile};
use pretty_assertions::assert_eq;

fn fixture(rel: &str) -> PathBuf {
    // MEGAMIX_FIXTURES lets cross-compiled test binaries run on another machine.
    std::env::var_os("MEGAMIX_FIXTURES")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures"))
        .join("thor")
        .join(rel)
}

const FIXTURES: &[&str] = &[
    "config.toml",
    "mods/ExPatch/config.toml",
    "mods/LyricsPatch/config.toml",
];

#[test]
fn fixtures_roundtrip_byte_identical() {
    for rel in FIXTURES {
        let path = fixture(rel);
        let original = fs::read(&path).unwrap();
        let f = TomlFile::load(&path).unwrap();
        assert!(!f.is_modified(), "{rel} reported as modified");
        assert_eq!(f.to_bytes(), original, "{rel} did not round-trip");
    }
}

#[test]
fn loader_config_reads_thor_values() {
    let c = LoaderConfig::load(fixture("config.toml")).unwrap();
    assert!(c.enabled().unwrap());
    assert!(c.console().unwrap());
    assert_eq!(c.mods_dir().unwrap(), "mods");
    assert_eq!(c.priority().unwrap(), ["LyricsPatch", "ExPatch"]);
}

#[test]
fn reorder_priority_changes_only_that_line() {
    let mut c = LoaderConfig::load(fixture("config.toml")).unwrap();
    c.set_priority(&["ExPatch".into(), "LyricsPatch".into()]);
    let before = fs::read_to_string(fixture("config.toml")).unwrap();
    let after = String::from_utf8(c.file.to_bytes()).unwrap();
    assert_eq!(
        after,
        before.replace(
            r#"priority = ["LyricsPatch", "ExPatch"]"#,
            r#"priority = ["ExPatch", "LyricsPatch"]"#
        )
    );
}

#[test]
fn lyricspatch_crlf_and_commented_include_survive_disable() {
    let path = fixture("mods/LyricsPatch/config.toml");
    let mut m = ModConfig::load(&path).unwrap();
    assert_eq!(m.file.line_ending(), LineEnding::CrLf);
    assert_eq!(m.include().unwrap(), None, "#include is a comment");
    assert_eq!(m.dll().unwrap(), ["LyricsPatch.dll"]);
    m.set_enabled(false);
    let before = fs::read_to_string(&path).unwrap();
    let after = String::from_utf8(m.file.to_bytes()).unwrap();
    assert_eq!(
        after,
        before.replacen("enabled = true\r\n", "enabled = false\r\n", 1)
    );
}

#[test]
fn expatch_settings_and_comments() {
    let path = fixture("mods/ExPatch/config.toml");
    let mut m = ModConfig::load(&path).unwrap();
    assert_eq!(m.name().unwrap(), Some("ExPatch"));
    assert_eq!(m.author().unwrap(), Some("nas, Stewie2.0"));
    assert_eq!(m.include().unwrap(), Some(vec![".".to_owned()]));
    let settings: Vec<_> = m.settings().map(|(k, _)| k.to_owned()).collect();
    assert_eq!(settings, ["permanent"]);

    m.set_setting("permanent", true.into());
    let before = fs::read_to_string(&path).unwrap();
    let after = String::from_utf8(m.file.to_bytes()).unwrap();
    assert_eq!(
        after,
        before.replace("permanent = false", "permanent = true")
    );
    assert!(after.contains("# Enable at your own risk.\n"));
}
