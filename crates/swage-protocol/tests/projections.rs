//! Regenerates the committed projections of the wire contract.
//!
//! Running `cargo test -p swage-protocol` rewrites both projections in
//! place, so drift between the Rust types and the committed artifacts
//! shows up as a dirty working tree rather than a silent mismatch:
//!
//! - TypeScript bindings → `frontend/src/lib/protocol/`
//! - JSON Schema (canonical artifact) → `crates/swage-protocol/schema/`

use std::path::{Path, PathBuf};

use ts_rs::TS;

use swage_protocol::client;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crate lives two levels below the repo root")
        .to_path_buf()
}

#[test]
fn typescript_bindings_are_regenerated() {
    let cfg = ts_rs::Config::new().with_out_dir(repo_root().join("frontend/src/lib/protocol"));

    // Exporting the top-level types pulls in every dependency they
    // reference, so the full surface lands in one pass.
    client::schedule::CreateScheduleRequest::export_all(&cfg).expect("export create");
    client::schedule::UpdateScheduleRequest::export_all(&cfg).expect("export update");
    client::schedule::Schedule::export_all(&cfg).expect("export schedule");
    client::schedule::ScheduleListResponse::export_all(&cfg).expect("export list");
    client::schedule::ScheduleRunListResponse::export_all(&cfg).expect("export runs");
    client::schedule::TriggerScheduleResponse::export_all(&cfg).expect("export trigger");
}

#[test]
fn client_schema_is_regenerated() {
    let schema = schemars::schema_for!(client::ClientSurface);
    let rendered = serde_json::to_string_pretty(&schema).expect("schema serialises") + "\n";

    let schema_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("schema/client.json");
    std::fs::create_dir_all(schema_path.parent().expect("schema dir")).expect("create schema dir");
    std::fs::write(&schema_path, rendered).expect("write schema document");
}
