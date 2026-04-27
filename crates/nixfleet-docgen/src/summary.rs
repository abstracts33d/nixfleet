//! Generate `SUMMARY.md` for the mdbook by walking the `manual/`
//! and `generated/` subtrees.
//!
//! Both subtrees use the same convention: any `.md` file is a
//! chapter; directories with the same name as a `.md` file at the
//! same level become its sub-pages (mdbook's nested layout).
//!
//! Output is sorted alphabetically per directory so reruns are
//! byte-identical. Manual content is emitted before generated
//! content — the curated narrative leads, the auto-extracted
//! reference follows.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

pub fn run(book_src: &Path) -> Result<()> {
    let manual = book_src.join("manual");
    let generated = book_src.join("generated");

    let mut s = String::new();
    s.push_str("# Summary\n\n");

    // Optional top-level introduction page, conventionally
    // `manual/README.md`. mdbook expects it linked as the prefix
    // chapter (no list bullet).
    let intro = manual.join("README.md");
    if intro.exists() {
        s.push_str("[Introduction](manual/README.md)\n\n");
    }

    if manual.exists() {
        s.push_str("# Manual\n\n");
        emit_section(&mut s, &manual, "manual", 0)?;
    }

    if generated.exists() {
        s.push_str("\n# Reference (auto-generated)\n\n");
        emit_section(&mut s, &generated, "generated", 0)?;
    }

    let out = book_src.join("SUMMARY.md");
    fs::write(&out, s)
        .with_context(|| format!("write {}", out.display()))?;
    Ok(())
}

fn emit_section(out: &mut String, dir: &Path, rel_prefix: &str, depth: usize) -> Result<()> {
    let mut entries: Vec<PathBuf> = fs::read_dir(dir)
        .with_context(|| format!("read {}", dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .collect();
    entries.sort();

    // Build a set of (stem -> (md, dir)) so we can render mdbook's
    // "page + nested children" pattern: `page.md` plus a sibling
    // `page/` directory get one bullet, the directory's contents
    // become its sub-bullets.
    let mut md_files: Vec<PathBuf> = Vec::new();
    let mut subdirs: Vec<PathBuf> = Vec::new();
    for e in entries {
        if e.is_file() && e.extension().and_then(|s| s.to_str()) == Some("md") {
            md_files.push(e);
        } else if e.is_dir() {
            subdirs.push(e);
        }
    }

    // Skip README.md at the top (already linked as introduction).
    md_files.retain(|p| {
        !(depth == 0
            && p.file_name().and_then(|n| n.to_str()) == Some("README.md")
            && rel_prefix == "manual")
    });

    for md in &md_files {
        let stem = md.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        let title = title_from_md(md).unwrap_or_else(|| stem.to_string());
        let rel = md
            .strip_prefix(md.ancestors().nth(depth + 2).unwrap_or(md))
            .unwrap_or(md);
        let _ = rel; // silence
        let link = format_relative_link(md, rel_prefix);
        let indent = "  ".repeat(depth);
        out.push_str(&format!("{indent}- [{title}]({link})\n"));

        // If there's a sibling dir with the same stem, recurse into
        // it as nested children.
        if let Some(parent) = md.parent() {
            let candidate = parent.join(stem);
            if candidate.is_dir() {
                emit_section(out, &candidate, rel_prefix, depth + 1)?;
                subdirs.retain(|d| d != &candidate);
            }
        }
    }

    // Any remaining subdirectory without a sibling .md is rendered
    // as a heading-style group (mdbook supports `# Heading` between
    // bullets at any depth, but we use a plain bullet labelled
    // after the directory name, then recurse).
    for sub in &subdirs {
        let name = sub.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let indent = "  ".repeat(depth);
        out.push_str(&format!("{indent}- {name}/\n"));
        emit_section(out, sub, rel_prefix, depth + 1)?;
    }

    Ok(())
}

fn title_from_md(path: &Path) -> Option<String> {
    let body = fs::read_to_string(path).ok()?;
    for line in body.lines() {
        let l = line.trim();
        if let Some(rest) = l.strip_prefix("# ") {
            return Some(rest.trim_matches('`').to_string());
        }
    }
    None
}

/// Compose the path that mdbook expects: relative to `src/`. Walk
/// ancestors until we hit the `manual` or `generated` segment,
/// then prepend that segment.
fn format_relative_link(md: &Path, prefix: &str) -> String {
    // Find prefix in ancestors and chop at it.
    let mut comps: Vec<String> = Vec::new();
    let mut found = false;
    for c in md.components() {
        if let Some(s) = c.as_os_str().to_str() {
            if !found && s == prefix {
                found = true;
                comps.push(s.to_string());
                continue;
            }
            if found {
                comps.push(s.to_string());
            }
        }
    }
    comps.join("/")
}
