# `nixfleet_docgen`

`nixfleet-docgen` — extract Markdown reference docs from nixfleet's
Rust + Nix sources.

Three subcommands:

- `rust <repo-root> <out-dir>`: walk `<repo-root>/crates/*/src/`,
  parse each `.rs` with `syn`, emit one `.md` per source file
  into `<out-dir>/rust/<crate>/<path>.md`. Includes ALL items
  (public + private). Skips `#[cfg(test)]` modules.

- `nix-comments <repo-root> <out-dir>`: walk a curated list of
  `.nix` files (lib/ + modules/scopes/nixfleet/), extract leading
  file-level comments + per-binding `# `-comments, emit `.md`.

- `summary <book-src-dir>`: walk `<book-src-dir>/manual/` and
  `<book-src-dir>/generated/`, emit a deterministic `SUMMARY.md`.

Determinism: every directory walk is sorted, every output file is
written atomically, no timestamps appear in the rendered Markdown.
Running twice against the same source tree produces byte-identical
files. CI runs `docs-check` (regenerate to a tmpdir + diff against
committed); a non-empty diff fails the build.

## Items

### 🔒 `fn ensure_clean_dir`

Ensure `dir` exists and is empty. Used by each subcommand before
emitting fresh files so stale output from a prior run can never
silently survive a rename/delete in the source tree.


