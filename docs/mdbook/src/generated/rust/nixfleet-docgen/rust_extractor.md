# `nixfleet_docgen::rust_extractor`

Rust source → Markdown extractor.

Walks `<repo>/crates/*/src/`, parses each `.rs` with `syn`, emits
`<out>/<crate>/<module-path>.md`. Coverage includes all items
(public + private) but skips:

- `#[cfg(test)] mod tests { … }` blocks (test-only code, doesn't
  belong in framework reference)
- The bodies of inline modules (the items inside a `mod foo { … }`
  declared inline in another file's source — those are still
  reachable, but for our codebase every module is its own file
  so the inline case is rare and best linked via "see source").

Output structure mirrors the source tree:

```text
crates/foo/src/lib.rs            → out/foo/index.md
crates/foo/src/main.rs           → out/foo/main.md
crates/foo/src/bar.rs            → out/foo/bar.md
crates/foo/src/bar/mod.rs        → out/foo/bar.md      (same target — mod.rs IS bar's docs)
crates/foo/src/bar/baz.rs        → out/foo/bar/baz.md
```

Each output file:

- H1 with the module path (`# nixfleet-control-plane::server`)
- The file's `//!` (module-level) docs
- Per-item section: H2 with the item kind + name, the item's
  `///` docs, and the syn-rendered signature in a code fence.

Items are emitted in **source order**, not alphabetical. Source
order tells the reader the same story they'd get from reading the
file top-to-bottom — useful for understanding intent. Determinism
is preserved because the source bytes are fixed.

## Items

### 🔒 `const SKIP_CRATES`

Crate names that have no Rust source worth documenting. Currently
none, but reserved if future infra crates appear.


### 🔓 `fn run`

_(no doc comment)_


### 🔒 `fn module_path_for`

`crates/foo/src/bar/baz.rs` (relative `bar/baz.rs`) → `foo::bar::baz`.
`lib.rs` / `main.rs` → bare crate name.
`mod.rs` is treated as the parent dir (`bar/mod.rs` → `foo::bar`).


### 🔒 `fn output_path_for`

Output `.md` path that mirrors the source tree.
`lib.rs` / `main.rs` → `index.md` / `main.md`.
`foo/mod.rs` → `foo.md` (the mod's docs file IS the parent
directory's name + `.md`, not `foo/mod.md`).
`foo/bar.rs` → `foo/bar.md`.


### 🔒 `fn collect_doc_lines`

Pull `///` (and `//!`) doc comments off an attribute list and
stitch them back together as Markdown. Filters out non-doc attrs.
Preserves blank lines between paragraphs.


### 🔒 `fn render_item`

Render one top-level item as a Markdown section. Returns `None`
for items we intentionally skip (test modules, use statements,
extern crates).


### 🔒 `fn vis_marker`

`pub` / `pub(crate)` / `pub(super)` / private. Marked in the
header so the reader can tell at a glance.


### 🔒 `fn render_impl_block`

Render an `impl` block: header line + one entry per associated
item that carries a doc comment. impls without any doc-bearing
items return `None` so we don't pollute the output with empty
trait conformances (Default, Debug, …).


