//! Exercises the real projection writers and their on-disk output.

use std::borrow::Cow;

use super::*;

fn assert_projection_bytes(name: &str, expected: &[u8]) {
    let actual = std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("schema")
            .join(name),
    )
    .expect("read regenerated projection");
    assert!(actual == expected, "JSON projection bytes changed: {name}");
}

#[test]
fn client_schema_bytes_are_stable() {
    client_schema_is_regenerated();
    assert_projection_bytes("client.json", include_bytes!("../../schema/client.json"));
}

#[test]
fn client_openapi_bytes_are_stable() {
    client_route_projections_are_regenerated();
    assert_projection_bytes(
        "client-openapi.json",
        include_bytes!("../../schema/client-openapi.json"),
    );
}

#[test]
fn channel_schema_bytes_are_stable() {
    channel_schema_is_regenerated();
    assert_projection_bytes("channel.json", include_bytes!("../../schema/channel.json"));
}

#[test]
fn device_schema_bytes_are_stable() {
    device_schema_is_regenerated();
    assert_projection_bytes("device.json", include_bytes!("../../schema/device.json"));
}

#[test]
fn hosting_schema_bytes_are_stable() {
    hosting_schema_is_regenerated();
    assert_projection_bytes("hosting.json", include_bytes!("../../schema/hosting.json"));
}

struct NestedProjection;

impl schemars::JsonSchema for NestedProjection {
    fn schema_name() -> Cow<'static, str> {
        "NestedProjection".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        serde_json::from_str::<serde_json::Value>(r#"{"z":{"z":2,"a":1},"a":[{"z":4,"a":3},2,1]}"#)
            .expect("valid nested JSON")
            .try_into()
            .expect("object schema")
    }
}

#[test]
fn nested_object_keys_are_sorted_and_arrays_keep_their_order() {
    let directory = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_TARGET_TMPDIR")))
        .join(format!("projection-output-{}", std::process::id()));
    std::fs::create_dir_all(&directory).expect("create projection test directory");
    let path = directory.join("nested.json");
    // An absolute destination exercises the schema writer without touching
    // the published documents alongside the other regeneration tests.
    write_schema::<NestedProjection>(path.to_str().expect("UTF-8 test path"));
    let actual = std::fs::read_to_string(&path).expect("read nested projection");
    std::fs::remove_dir_all(&directory).expect("remove projection test directory");

    // Parsing a sorted document builds the expected order under either map
    // implementation; schema_for also supplies the dialect and title.
    let expected: serde_json::Value = serde_json::from_str(
        r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","a":[{"a":3,"z":4},2,1],"title":"NestedProjection","z":{"a":1,"z":2}}"#,
    )
    .expect("valid expected projection");
    assert_eq!(
        actual,
        serde_json::to_string_pretty(&expected).expect("expected JSON serialises") + "\n",
        "nested keys must be sorted without reordering array elements",
    );
}
