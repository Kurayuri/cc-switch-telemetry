# cc-switch session parser snapshot

This directory contains a source snapshot used to derive and audit
`crates/session-usage-core`. The six parser modules below were copied byte for
byte from cc-switch commit
`3217f72596f2d1c0f879f0a05f83803825d9809f` (v3.20.1).

| File | SHA-256 |
| --- | --- |
| `src-tauri/src/services/session_usage.rs` | `7ae348e46f259d19195d8de4f42bea48dd6963ad77441aff94d05d52c44621f9` |
| `src-tauri/src/services/session_usage_codex.rs` | `eadb325ca4b408c9a330784d3e6e6bca86b235cf0e5ce9a4568c82596e055d03` |
| `src-tauri/src/services/session_usage_gemini.rs` | `a5b158271a984325d29a6b3fbba99429d96a9729482c99d64cf73cbd82dbf727` |
| `src-tauri/src/services/session_usage_grokbuild.rs` | `269f0b3250a89b5d562fa1bab41ea1c4aedb7f61f50af961712697b8a8adba55` |
| `src-tauri/src/services/session_usage_opencode.rs` | `5b423f4deffab6f330e1dfb68852d3a72e26f6c0095417935a1db5545f4bb516` |
| `src-tauri/src/services/session_usage_pi.rs` | `8e8065b8dcca900ebb70ea0e9d47401176a88e2145a3fe105730061e6ef019f8` |

The snapshot preserves the complete application-integrated implementation,
including cursor and database behavior. It is not compiled directly because
those modules depend on cc-switch's Tauri/database/config layers. The telemetry
crate owns a Tauri-free adapter derived from this snapshot and tests its
six-source behavior separately.
