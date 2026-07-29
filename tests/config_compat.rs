//! Backward-compatibility checks for removed config options.
//!
//! Lives in an integration test (not `src/config.rs`) so removed option
//! names stay out of `src/` entirely.

use brunson::config::Config;

/// The `tui.diff_style` option was removed (its advertised "side-by-side"
/// value was never implemented). Existing user configs that still carry the
/// key must keep parsing: serde-toml ignores unknown fields.
#[test]
fn config_parses_with_removed_diff_style_key() {
    let dir = std::env::temp_dir().join(format!(
        "brunson-config-compat-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("config.toml");
    std::fs::write(
        &path,
        r#"
[github]
watch = []

[daemon]
port = 17890

[tui]
diff_style = "unified"
show_line_numbers = true
"#,
    )
    .unwrap();

    let config = Config::load(Some(&path)).expect("config with removed key still parses");
    assert!(config.tui.show_line_numbers);

    std::fs::remove_dir_all(&dir).ok();
}
