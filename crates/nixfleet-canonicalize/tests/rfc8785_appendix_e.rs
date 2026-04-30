//! RFC 8785 Appendix E conformance corpus.
//!
//! The pinned `serde_jcs` library is the source of truth for our
//! signature contract — every signer + verifier in the fleet
//! depends on it. A future version bump that silently changes
//! corner-case behaviour would drift signature bytes for every
//! existing artifact and break verification.
//!
//! These test vectors codify the JCS rules from RFC 8785
//! Appendix E directly, so a regression in the underlying library
//! fires before it lands in production. The cases cover the
//! load-bearing edge cases: array preservation, object key
//! sorting (UTF-16 code unit order, NOT locale-aware), nested
//! structure recursion, control-character escaping, and empty
//! containers. Numeric edge cases (RFC 8785 §3.2.2.2) are not
//! exercised here — the framework's producer side disallows floats
//! at the schema layer (see `lib/mk-fleet.nix`), so number
//! canonicalisation is not on the signature path.
//!
//! Adding a new case: append `(input, expected)` to `CASES`. The
//! test iterates and reports the first divergence with both
//! produced + expected for diff-friendly output.
//!
//! References:
//! - RFC 8785 §3 (canonical JSON form rules)
//! - RFC 8785 Appendix E (test vectors)
//! - <https://github.com/cyberphone/json-canonicalization> (reference impl)

use nixfleet_canonicalize::canonicalize;

/// Each entry is `(name, input_json, expected_canonical_json)`.
/// `name` shows up in the failure message. `input_json` is the
/// non-canonical form a producer might emit (or the canonical
/// form re-fed to test idempotence). `expected_canonical_json` is
/// the byte-exact canonical output the framework must produce.
const CASES: &[(&str, &str, &str)] = &[
    // Empty containers (RFC 8785 §3.2.1).
    ("empty_object", "{}", "{}"),
    ("empty_array", "[]", "[]"),
    // Object key sorting (RFC 8785 §3.2.3 — UTF-16 code unit
    // order). Keys-already-sorted inputs round-trip stable.
    (
        "keys_sorted_simple",
        r#"{"a":1,"b":2}"#,
        r#"{"a":1,"b":2}"#,
    ),
    // Keys-out-of-order get re-ordered.
    (
        "keys_unsorted",
        r#"{"b":2,"a":1}"#,
        r#"{"a":1,"b":2}"#,
    ),
    // Numeric keys sort as strings, NOT numerically. "10" < "2"
    // because '1' (0x31) < '2' (0x32). Matches RFC 8785 Appendix
    // E.1 nested-object behaviour.
    (
        "numeric_keys_sort_lexicographically",
        r#"{"2":"two","10":"ten","1":"one"}"#,
        r#"{"1":"one","10":"ten","2":"two"}"#,
    ),
    // Arrays preserve insertion order (RFC 8785 §3.2.2.1) — only
    // object keys sort. Mixing array-with-objects exercises both
    // rules together. Direct adaptation of RFC 8785 Appendix E.1.
    (
        "rfc8785_e1_arrays",
        r#"[56,{"d":true,"10":null,"1":[]}]"#,
        r#"[56,{"1":[],"10":null,"d":true}]"#,
    ),
    // Nested objects recurse with the same key-sort rule.
    (
        "nested_objects_sort_recursively",
        r#"{"outer":{"z":1,"a":2}}"#,
        r#"{"outer":{"a":2,"z":1}}"#,
    ),
    // Control characters in strings escape per RFC 8785 §3.2.2.2.
    // Backspace = U+0008, tab = U+0009, newline = U+000A.
    (
        "control_chars_escaped",
        "{\"k\":\"\\b\\t\\n\"}",
        r#"{"k":"\b\t\n"}"#,
    ),
    // Forward slash is NOT escaped (only required by JSON
    // grammar, not by JCS canonical form).
    (
        "forward_slash_not_escaped",
        r#"{"url":"http:\/\/example.com"}"#,
        r#"{"url":"http://example.com"}"#,
    ),
    // Booleans + null round-trip.
    (
        "primitives_round_trip",
        r#"{"t":true,"f":false,"n":null}"#,
        r#"{"f":false,"n":null,"t":true}"#,
    ),
];

#[test]
fn rfc8785_appendix_e_corpus() {
    let mut failures = Vec::new();
    for (name, input, expected) in CASES {
        let produced = match canonicalize(input) {
            Ok(p) => p,
            Err(err) => {
                failures.push(format!("{name}: canonicalize errored: {err:?}"));
                continue;
            }
        };
        if produced != *expected {
            failures.push(format!(
                "{name}:\n  input    = {input}\n  produced = {produced}\n  expected = {expected}",
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "RFC 8785 Appendix E corpus mismatches ({} of {}):\n{}",
        failures.len(),
        CASES.len(),
        failures.join("\n\n"),
    );
}

/// Idempotence: every canonical output is a fixed point of
/// canonicalize. Any drift here is the same library-version-bump
/// regression the corpus is meant to catch — but exercised on the
/// canonical bytes we just produced rather than the input bytes.
#[test]
fn corpus_canonical_outputs_are_fixed_points() {
    for (name, _input, expected) in CASES {
        let twice = canonicalize(expected)
            .unwrap_or_else(|err| panic!("{name}: re-canonicalize expected failed: {err:?}"));
        assert_eq!(
            twice, *expected,
            "{name}: canonical form is not a fixed point",
        );
    }
}
