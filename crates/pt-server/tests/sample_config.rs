//! `tests/sample-config.json` is what scripts/seed-server.mjs uploads as the
//! client's config: the sample config's canonical JSON and integrity hash.
//! Regenerate with `UPDATE_SAMPLE_CONFIG=1 cargo test -p pt-server --test sample_config`.

use std::path::Path;

#[test]
fn seed_script_config_matches_the_sample() {
    let cfg = pt_server::config::sample();
    let want = serde_json::json!({
        "hash": cfg.integrity_hash(),
        "config": serde_json::from_str::<serde_json::Value>(&cfg.integrity_canonical_json()).unwrap(),
    });
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/sample-config.json");
    if std::env::var_os("UPDATE_SAMPLE_CONFIG").is_some() {
        std::fs::write(&path, serde_json::to_string_pretty(&want).unwrap() + "\n").unwrap();
    }
    let have: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(have, want, "run with UPDATE_SAMPLE_CONFIG=1 after changing the sample config");
}
