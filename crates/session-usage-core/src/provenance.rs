use crate::IMPORTER_SOURCE_COMMIT;
use sha2::{Digest, Sha256};
use std::{fs, path::Path};

const SNAPSHOT_FILES: [(&str, &str); 6] = [
    (
        "session_usage.rs",
        "f159e9ebb92fc57d99070cedc479a29ba0b3a1a42653c426b81a6725bd0f0e9e",
    ),
    (
        "session_usage_codex.rs",
        "8a2925ede70a1603068bb97b4dbb177f45cea3c6649c6845307c1d5ea687315c",
    ),
    (
        "session_usage_gemini.rs",
        "a5b158271a984325d29a6b3fbba99429d96a9729482c99d64cf73cbd82dbf727",
    ),
    (
        "session_usage_grokbuild.rs",
        "269f0b3250a89b5d562fa1bab41ea1c4aedb7f61f50af961712697b8a8adba55",
    ),
    (
        "session_usage_opencode.rs",
        "5b423f4deffab6f330e1dfb68852d3a72e26f6c0095417935a1db5545f4bb516",
    ),
    (
        "session_usage_pi.rs",
        "8e8065b8dcca900ebb70ea0e9d47401176a88e2145a3fe105730061e6ef019f8",
    ),
];

#[test]
fn vendored_parser_snapshot_matches_declared_source() {
    assert_eq!(
        IMPORTER_SOURCE_COMMIT,
        "87d966b7f887adfe0e9856ee0f7e93cc8efc874f"
    );
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../3rdparty/cc-switch/src-tauri/src/services");
    for (name, expected) in SNAPSHOT_FILES {
        let bytes = fs::read(root.join(name)).unwrap();
        assert_eq!(format!("{:x}", Sha256::digest(bytes)), expected, "{name}");
    }
}
