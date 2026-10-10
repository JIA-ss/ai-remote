//! The crate's canonical exports for embedders: the in-crate transport-vocabulary
//! surface-leak fixture, and the crate version (the release-train anchor bundlers pin).

#[test]
fn transport_vocabulary_parses_with_the_three_term_lists() {
    let v: serde_json::Value = serde_json::from_str(mcpmesh_local_api::TRANSPORT_VOCABULARY)
        .expect("fixture is valid JSON");
    for key in ["substring_banned", "token_banned", "carve_outs"] {
        assert!(
            v[key].as_array().is_some_and(|a| !a.is_empty()),
            "{key} missing or empty"
        );
    }
}

#[test]
fn version_is_the_crate_version() {
    assert_eq!(mcpmesh_local_api::VERSION, env!("CARGO_PKG_VERSION"));
}
