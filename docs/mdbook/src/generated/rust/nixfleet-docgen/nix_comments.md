# `nixfleet_docgen::nix_comments`

Nix source → Markdown extractor.

Walks a curated set of `.nix` files (the framework surface — not
the test fixtures or examples) and emits one `.md` per file
containing:

- The leading file-level comments (every `# ` line at the top of
  the file before any code begins).
- Per-binding comments: `# `-comments immediately above a
  top-level `<name> = … ;` form. Captures both `lib`-style
  bindings and `mkOption` declarations.

No Nix parser involved — line-based extraction only. Means we
handle malformed-but-readable files without choking, and means
`nix run .#docs` works without a Nix evaluation step. Trade-off:
we don't render rendered option types (the `nixosOptionsDoc` path
would do that, deferred to a follow-up).

## Items

### 🔒 `const NIX_TARGETS`

`.nix` files we extract docs from. Glob-relative to repo root.
Order is meaningful: it determines SUMMARY.md sort order within
the Nix subtree.


### 🔓 `fn run`

_(no doc comment)_


### 🔒 `fn output_path_for`

`lib/mkFleet.nix` → `out/lib/mkFleet.md`.
`modules/scopes/nixfleet/_agent.nix` → `out/modules/scopes/nixfleet/_agent.md`.


### 🔒 `fn extract_header_comments`

Pull the leading run of `# …` comment lines off the top of the
file. Stops at the first non-comment line that isn't blank.
Returns the rejoined Markdown body (one paragraph, line breaks
preserved).


### 🔒 `fn extract_bindings_with_comments`

Walk the file looking for `<ident> = …;` or `<ident> = mkOption {`
at zero brace-depth (i.e. top-level of an attribute set). For each
such binding, capture the run of `# `-comment lines immediately
above as its docstring. Returns a Vec preserving source order.


### 🔒 `fn parse_binding_name`

Parse a line like `foo = …;` or `foo.bar.baz = …;` and return
`foo` (or `foo.bar.baz`). None if the line doesn't match a
binding shape.


