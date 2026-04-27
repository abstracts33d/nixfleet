//! Nix source → Markdown extractor.
//!
//! Walks a curated set of `.nix` files (the framework surface — not
//! the test fixtures or examples) and emits one `.md` per file
//! containing:
//!
//! - The leading file-level comments (every `# ` line at the top of
//!   the file before any code begins).
//! - Per-binding comments: `# `-comments immediately above a
//!   top-level `<name> = … ;` form. Captures both `lib`-style
//!   bindings and `mkOption` declarations.
//!
//! No Nix parser involved — line-based extraction only. Means we
//! handle malformed-but-readable files without choking, and means
//! `nix run .#docs` works without a Nix evaluation step. Trade-off:
//! we don't render rendered option types (the `nixosOptionsDoc` path
//! would do that, deferred to a follow-up).

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// `.nix` files we extract docs from. Glob-relative to repo root.
/// Order is meaningful: it determines SUMMARY.md sort order within
/// the Nix subtree.
const NIX_TARGETS: &[&str] = &[
    "lib/default.nix",
    "lib/mkFleet.nix",
    "modules/_shared/lib/mk-host.nix",
    "modules/_shared/host-spec-module.nix",
    "modules/scopes/nixfleet/_agent.nix",
    "modules/scopes/nixfleet/_agent_darwin.nix",
    "modules/scopes/nixfleet/_cache-server.nix",
    "modules/scopes/nixfleet/_cache.nix",
    "modules/scopes/nixfleet/_control-plane.nix",
    "modules/scopes/nixfleet/_microvm-host.nix",
    "modules/scopes/nixfleet/_operator.nix",
    "modules/scopes/nixfleet/_trust-json.nix",
    "modules/_trust.nix",
];

pub fn run(repo_root: &Path, out_dir: &Path) -> Result<()> {
    fs::create_dir_all(out_dir)
        .with_context(|| format!("create {}", out_dir.display()))?;

    for rel in NIX_TARGETS {
        let src = repo_root.join(rel);
        if !src.exists() {
            // A target may legitimately not exist on a branch where
            // the file was renamed or removed. Skip silently — the
            // operator will see the gap in SUMMARY.md and update
            // NIX_TARGETS in a follow-up commit.
            continue;
        }
        let body = fs::read_to_string(&src)
            .with_context(|| format!("read {}", src.display()))?;
        let md = render_nix_file(rel, &body);
        let out_path = output_path_for(out_dir, rel);
        if let Some(parent) = out_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        fs::write(&out_path, md)
            .with_context(|| format!("write {}", out_path.display()))?;
    }
    Ok(())
}

/// `lib/mkFleet.nix` → `out/lib/mkFleet.md`.
/// `modules/scopes/nixfleet/_agent.nix` → `out/modules/scopes/nixfleet/_agent.md`.
fn output_path_for(out_dir: &Path, rel: &str) -> PathBuf {
    let mut p = out_dir.to_path_buf();
    let path = Path::new(rel);
    if let Some(parent) = path.parent() {
        for c in parent.components() {
            if let Some(s) = c.as_os_str().to_str() {
                p.push(s);
            }
        }
    }
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("file");
    p.push(format!("{stem}.md"));
    p
}

fn render_nix_file(rel: &str, src: &str) -> String {
    let mut out = String::new();
    out.push_str(&format!("# `{rel}`\n\n"));

    // File-level comments: every leading `# `-line, stopping at
    // first non-comment non-blank line.
    let header = extract_header_comments(src);
    if !header.is_empty() {
        out.push_str(&header);
        out.push('\n');
    }

    // Only emit bindings that carry an actual `# ` doc comment.
    // Nix files have far more `<name> = …;` lines than meaningful
    // names — every nested attribute key looks like a binding to a
    // line-based parser. Filtering by "has a doc above it" reliably
    // surfaces the documented surface (option declarations, library
    // functions) and drops the noise.
    let bindings: Vec<(String, String)> = extract_bindings_with_comments(src)
        .into_iter()
        .filter(|(_, doc)| !doc.trim().is_empty())
        .collect();
    if !bindings.is_empty() {
        out.push_str("## Bindings\n\n");
        for (name, doc) in bindings {
            out.push_str(&format!("### `{name}`\n\n"));
            out.push_str(&doc);
            if !doc.ends_with('\n') {
                out.push('\n');
            }
            out.push('\n');
        }
    } else if header.is_empty() {
        out.push_str("_No file-level docs or documented bindings._\n");
    }

    out
}

/// Pull the leading run of `# …` comment lines off the top of the
/// file. Stops at the first non-comment line that isn't blank.
/// Returns the rejoined Markdown body (one paragraph, line breaks
/// preserved).
fn extract_header_comments(src: &str) -> String {
    let mut out = Vec::new();
    for line in src.lines() {
        let trimmed = line.trim_start();
        if trimmed.is_empty() {
            // Allow blank lines inside the leading comment block.
            if !out.is_empty() {
                out.push(String::new());
            }
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("# ") {
            out.push(rest.to_string());
        } else if trimmed == "#" {
            out.push(String::new());
        } else {
            break;
        }
    }
    // Trim trailing empties.
    while out.last().map(|s| s.is_empty()).unwrap_or(false) {
        out.pop();
    }
    if out.is_empty() {
        String::new()
    } else {
        let mut s = out.join("\n");
        s.push('\n');
        s
    }
}

/// Walk the file looking for `<ident> = …;` or `<ident> = mkOption {`
/// at zero brace-depth (i.e. top-level of an attribute set). For each
/// such binding, capture the run of `# `-comment lines immediately
/// above as its docstring. Returns a Vec preserving source order.
fn extract_bindings_with_comments(src: &str) -> Vec<(String, String)> {
    let mut bindings = Vec::new();
    let mut pending_doc: Vec<String> = Vec::new();
    let mut brace_depth: i32 = 0;
    // We start "open" at the file-level binding tier; the outer
    // `{ … }` of a NixOS module increments depth to 1 around all
    // bindings, but we accept depth ∈ {0, 1} as "top-level for
    // doc purposes" — that covers both `let … in { … }` and
    // `{ config, lib, ...}: { … }` shapes.
    let mut entered_module_attrs = false;

    for line in src.lines() {
        let trimmed = line.trim();
        // Track braces (cheap textual; not robust to braces inside
        // strings but close enough for our cwd's hand-written files).
        let opens = trimmed.matches('{').count() as i32;
        let closes = trimmed.matches('}').count() as i32;

        if !entered_module_attrs && opens > closes {
            entered_module_attrs = true;
            brace_depth += opens - closes;
            pending_doc.clear();
            continue;
        }

        if let Some(rest) = trimmed.strip_prefix("# ") {
            pending_doc.push(rest.to_string());
        } else if trimmed == "#" {
            pending_doc.push(String::new());
        } else if trimmed.is_empty() {
            // Blank line — keep the pending block intact.
        } else {
            // Non-comment line. Check for binding shape at low depth.
            let effective_depth = brace_depth - opens + closes;
            // We treat depth ≤ 1 as "outer attrs" (the module body).
            if effective_depth <= 1 {
                if let Some(name) = parse_binding_name(trimmed) {
                    let doc = pending_doc.join("\n");
                    bindings.push((name, doc));
                }
            }
            pending_doc.clear();
            brace_depth += opens - closes;
        }
    }
    bindings
}

/// Parse a line like `foo = …;` or `foo.bar.baz = …;` and return
/// `foo` (or `foo.bar.baz`). None if the line doesn't match a
/// binding shape.
fn parse_binding_name(line: &str) -> Option<String> {
    // Skip lines that obviously aren't bindings.
    if line.starts_with("let")
        || line.starts_with("in ")
        || line.starts_with("in{")
        || line == "in"
        || line.starts_with("with ")
        || line.starts_with("inherit ")
    {
        return None;
    }
    let eq_pos = line.find(" = ")?;
    let lhs = &line[..eq_pos];
    // LHS must look like a (possibly dotted) identifier.
    if lhs.is_empty() {
        return None;
    }
    if !lhs
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-' || c == '"')
    {
        return None;
    }
    Some(lhs.trim_matches('"').to_string())
}
