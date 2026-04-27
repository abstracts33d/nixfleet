//! Rust source → Markdown extractor.
//!
//! Walks `<repo>/crates/*/src/`, parses each `.rs` with `syn`, emits
//! `<out>/<crate>/<module-path>.md`. Coverage includes all items
//! (public + private) but skips:
//!
//! - `#[cfg(test)] mod tests { … }` blocks (test-only code, doesn't
//!   belong in framework reference)
//! - The bodies of inline modules (the items inside a `mod foo { … }`
//!   declared inline in another file's source — those are still
//!   reachable, but for our codebase every module is its own file
//!   so the inline case is rare and best linked via "see source").
//!
//! Output structure mirrors the source tree:
//!
//! ```text
//! crates/foo/src/lib.rs            → out/foo/index.md
//! crates/foo/src/main.rs           → out/foo/main.md
//! crates/foo/src/bar.rs            → out/foo/bar.md
//! crates/foo/src/bar/mod.rs        → out/foo/bar.md      (same target — mod.rs IS bar's docs)
//! crates/foo/src/bar/baz.rs        → out/foo/bar/baz.md
//! ```
//!
//! Each output file:
//!
//! - H1 with the module path (`# nixfleet-control-plane::server`)
//! - The file's `//!` (module-level) docs
//! - Per-item section: H2 with the item kind + name, the item's
//!   `///` docs, and the syn-rendered signature in a code fence.
//!
//! Items are emitted in **source order**, not alphabetical. Source
//! order tells the reader the same story they'd get from reading the
//! file top-to-bottom — useful for understanding intent. Determinism
//! is preserved because the source bytes are fixed.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use syn::{Attribute, ImplItem, Item, Visibility};

/// Crate names that have no Rust source worth documenting. Currently
/// none, but reserved if future infra crates appear.
const SKIP_CRATES: &[&str] = &[];

pub fn run(repo_root: &Path, out_dir: &Path) -> Result<()> {
    let crates_dir = repo_root.join("crates");
    let mut crates: Vec<PathBuf> = fs::read_dir(&crates_dir)
        .with_context(|| format!("read {}", crates_dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| !SKIP_CRATES.contains(&n))
                .unwrap_or(false)
        })
        .collect();
    crates.sort();

    for crate_root in crates {
        let crate_name = crate_root
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("<unknown>")
            .to_string();
        let src_dir = crate_root.join("src");
        if !src_dir.exists() {
            continue;
        }
        let crate_out = out_dir.join(&crate_name);
        process_crate(&crate_name, &src_dir, &crate_out)?;
    }
    Ok(())
}

fn process_crate(crate_name: &str, src_dir: &Path, crate_out: &Path) -> Result<()> {
    fs::create_dir_all(crate_out)
        .with_context(|| format!("create {}", crate_out.display()))?;

    // Sorted recursive walk — alphabetised paths so Rust's filesystem
    // order can't sneak nondeterminism in.
    let mut files: Vec<PathBuf> = walkdir::WalkDir::new(src_dir)
        .sort_by_file_name()
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.path().to_path_buf())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("rs"))
        .collect();
    files.sort();

    for file in files {
        let rel = file
            .strip_prefix(src_dir)
            .with_context(|| format!("strip prefix {}", file.display()))?;
        let module_path = module_path_for(crate_name, rel);
        let out_path = output_path_for(crate_out, rel);
        let md = render_file(&file, &module_path)?;
        if let Some(parent) = out_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        fs::write(&out_path, md)
            .with_context(|| format!("write {}", out_path.display()))?;
    }
    Ok(())
}

/// `crates/foo/src/bar/baz.rs` (relative `bar/baz.rs`) → `foo::bar::baz`.
/// `lib.rs` / `main.rs` → bare crate name.
/// `mod.rs` is treated as the parent dir (`bar/mod.rs` → `foo::bar`).
fn module_path_for(crate_name: &str, rel: &Path) -> String {
    let stem = rel.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    let parent_segments: Vec<String> = rel
        .parent()
        .map(|p| {
            p.components()
                .filter_map(|c| c.as_os_str().to_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let mut segments = vec![crate_name.replace('-', "_")];
    segments.extend(parent_segments);
    if !matches!(stem, "lib" | "main" | "mod") {
        segments.push(stem.to_string());
    }
    segments.join("::")
}

/// Output `.md` path that mirrors the source tree.
/// `lib.rs` / `main.rs` → `index.md` / `main.md`.
/// `foo/mod.rs` → `foo.md` (the mod's docs file IS the parent
/// directory's name + `.md`, not `foo/mod.md`).
/// `foo/bar.rs` → `foo/bar.md`.
fn output_path_for(crate_out: &Path, rel: &Path) -> PathBuf {
    let stem = rel.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    let parent = rel.parent().unwrap_or_else(|| Path::new(""));
    match stem {
        "lib" => crate_out.join("index.md"),
        "main" => crate_out.join("main.md"),
        "mod" => {
            // foo/bar/mod.rs → foo/bar.md (in `out/<crate>/`)
            let mut p = crate_out.to_path_buf();
            for c in parent.components() {
                if let Some(s) = c.as_os_str().to_str() {
                    p.push(s);
                }
            }
            p.set_extension("md");
            p
        }
        other => {
            let mut p = crate_out.to_path_buf();
            for c in parent.components() {
                if let Some(s) = c.as_os_str().to_str() {
                    p.push(s);
                }
            }
            p.push(format!("{other}.md"));
            p
        }
    }
}

fn render_file(path: &Path, module_path: &str) -> Result<String> {
    let src = fs::read_to_string(path)
        .with_context(|| format!("read {}", path.display()))?;
    let ast: syn::File = syn::parse_str(&src)
        .with_context(|| format!("parse {}", path.display()))?;

    let mut out = String::new();
    out.push_str(&format!("# `{module_path}`\n\n"));

    // File-level (`//!`) docs come from File.attrs filtered for #[doc=...].
    let file_doc = collect_doc_lines(&ast.attrs);
    if !file_doc.is_empty() {
        out.push_str(&file_doc);
        out.push('\n');
    }

    let mut had_items = false;
    for item in &ast.items {
        if let Some(rendered) = render_item(item) {
            if !had_items {
                out.push_str("## Items\n\n");
                had_items = true;
            }
            out.push_str(&rendered);
            out.push('\n');
        }
    }

    if !had_items && file_doc.is_empty() {
        out.push_str("_No items or module-level docs._\n");
    }

    Ok(out)
}

/// Pull `///` (and `//!`) doc comments off an attribute list and
/// stitch them back together as Markdown. Filters out non-doc attrs.
/// Preserves blank lines between paragraphs.
fn collect_doc_lines(attrs: &[Attribute]) -> String {
    let mut lines: Vec<String> = Vec::new();
    for attr in attrs {
        if !attr.path().is_ident("doc") {
            continue;
        }
        // `#[doc = "..."]` — we want the string literal.
        if let syn::Meta::NameValue(nv) = &attr.meta {
            if let syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(s),
                ..
            }) = &nv.value
            {
                let raw = s.value();
                // `///` desugars to ` <text>` (leading space). Strip
                // exactly one leading space if present so `# Heading`
                // and `- bullet` come out correctly.
                let trimmed = raw.strip_prefix(' ').unwrap_or(&raw);
                lines.push(trimmed.to_string());
            }
        }
    }
    if lines.is_empty() {
        String::new()
    } else {
        let mut s = lines.join("\n");
        if !s.ends_with('\n') {
            s.push('\n');
        }
        s
    }
}

/// Render one top-level item as a Markdown section. Returns `None`
/// for items we intentionally skip (test modules, use statements,
/// extern crates).
fn render_item(item: &Item) -> Option<String> {
    // Universal skips: use/extern/macro_rules invocations have no
    // useful surface. Test modules are handled below per `Item::Mod`.
    match item {
        Item::Use(_) | Item::ExternCrate(_) | Item::Macro(_) => return None,
        Item::Mod(m) if has_cfg_test(&m.attrs) => return None,
        _ => {}
    }

    let (kind, name, vis) = match item {
        Item::Fn(f) => ("fn", f.sig.ident.to_string(), &f.vis),
        Item::Struct(s) => ("struct", s.ident.to_string(), &s.vis),
        Item::Enum(e) => ("enum", e.ident.to_string(), &e.vis),
        Item::Trait(t) => ("trait", t.ident.to_string(), &t.vis),
        Item::Type(t) => ("type", t.ident.to_string(), &t.vis),
        Item::Const(c) => ("const", c.ident.to_string(), &c.vis),
        Item::Static(s) => ("static", s.ident.to_string(), &s.vis),
        Item::Impl(i) => {
            // impl blocks have no name; render as `impl Type` /
            // `impl Trait for Type`. Skip impls that have no
            // doc-bearing items inside.
            return render_impl_block(i);
        }
        Item::Mod(m) => ("mod", m.ident.to_string(), &m.vis),
        Item::Union(u) => ("union", u.ident.to_string(), &u.vis),
        Item::TraitAlias(a) => ("trait alias", a.ident.to_string(), &a.vis),
        // Verbatim, MacroN, etc. — too rare in our codebase to bother.
        _ => return None,
    };

    let attrs = item_attrs(item);
    let doc = collect_doc_lines(attrs);

    // Skip undocumented private items: the body is empty AND nobody
    // outside can use them, so they're noise. Public items show up
    // even without docs (their existence IS information).
    let is_pub = matches!(vis, Visibility::Public(_));
    if doc.is_empty() && !is_pub {
        return None;
    }

    let vis_marker = vis_marker(vis);
    let mut out = String::new();
    out.push_str(&format!("### {vis_marker}`{kind} {name}`\n\n"));
    if !doc.is_empty() {
        out.push_str(&doc);
        out.push('\n');
    } else {
        out.push_str("_(no doc comment)_\n\n");
    }
    Some(out)
}

/// `pub` / `pub(crate)` / `pub(super)` / private. Marked in the
/// header so the reader can tell at a glance.
fn vis_marker(vis: &Visibility) -> &'static str {
    match vis {
        Visibility::Public(_) => "🔓 ",
        Visibility::Restricted(_) => "🔐 ",
        Visibility::Inherited => "🔒 ",
    }
}

fn item_attrs(item: &Item) -> &[Attribute] {
    match item {
        Item::Fn(f) => &f.attrs,
        Item::Struct(s) => &s.attrs,
        Item::Enum(e) => &e.attrs,
        Item::Trait(t) => &t.attrs,
        Item::Type(t) => &t.attrs,
        Item::Const(c) => &c.attrs,
        Item::Static(s) => &s.attrs,
        Item::Impl(i) => &i.attrs,
        Item::Mod(m) => &m.attrs,
        Item::Union(u) => &u.attrs,
        Item::TraitAlias(a) => &a.attrs,
        _ => &[],
    }
}

fn has_cfg_test(attrs: &[Attribute]) -> bool {
    use quote::ToTokens;
    for a in attrs {
        if !a.path().is_ident("cfg") {
            continue;
        }
        // Cheap textual check: `#[cfg(test)]`, `#[cfg(all(test, …))]`,
        // etc. Anything with `test` in the cfg expression skips.
        let toks = a.meta.to_token_stream().to_string();
        if toks.contains("test") {
            return true;
        }
    }
    false
}

/// Render an `impl` block: header line + one entry per associated
/// item that carries a doc comment. impls without any doc-bearing
/// items return `None` so we don't pollute the output with empty
/// trait conformances (Default, Debug, …).
fn render_impl_block(i: &syn::ItemImpl) -> Option<String> {
    use quote::ToTokens;
    let header = {
        let mut s = String::new();
        if i.unsafety.is_some() {
            s.push_str("unsafe ");
        }
        s.push_str("impl");
        if let Some((_, path, _)) = &i.trait_ {
            s.push(' ');
            s.push_str(&path.to_token_stream().to_string());
            s.push_str(" for");
        }
        s.push(' ');
        s.push_str(&i.self_ty.to_token_stream().to_string());
        s
    };

    let mut sub: Vec<String> = Vec::new();
    for it in &i.items {
        let (name, attrs) = match it {
            ImplItem::Fn(f) => (f.sig.ident.to_string(), &f.attrs),
            ImplItem::Const(c) => (c.ident.to_string(), &c.attrs),
            ImplItem::Type(t) => (t.ident.to_string(), &t.attrs),
            _ => continue,
        };
        let doc = collect_doc_lines(attrs);
        if doc.is_empty() {
            continue;
        }
        let mut s = String::new();
        s.push_str(&format!("- **`{name}`** — "));
        s.push_str(doc.trim());
        s.push('\n');
        sub.push(s);
    }
    if sub.is_empty() {
        return None;
    }
    let mut out = String::new();
    out.push_str(&format!("### `{header}`\n\n"));
    for s in sub {
        out.push_str(&s);
    }
    Some(out)
}
