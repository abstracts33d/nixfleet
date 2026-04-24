//! Proto round-trip tests.
//!
//! Byte-exact: parse → re-serialize through JCS canonicalizer →
//! assert bytes match the committed golden.

use nixfleet_canonicalize::canonicalize;
use nixfleet_proto::FleetResolved;

fn load(path: &str) -> String {
    std::fs::read_to_string(format!("tests/fixtures/{path}"))
        .unwrap_or_else(|err| panic!("read fixture {path}: {err}"))
}

#[test]
fn every_nullable_roundtrips_byte_for_byte() {
    let input = load("every-nullable.json");
    let golden = load("every-nullable.canonical");

    let parsed: FleetResolved =
        serde_json::from_str(&input).expect("parse every-nullable.json");

    let reserialized = serde_json::to_string(&parsed).expect("serialize FleetResolved");
    let produced = canonicalize(&reserialized).expect("canonicalize reserialized");

    assert_eq!(
        produced, golden,
        "FleetResolved round-trip is not JCS byte-identical to Stream B-style emission"
    );
}
