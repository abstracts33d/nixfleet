# NixFleet

Declarative NixOS fleet management. Three layers, one wire protocol, no daemons on the agent path.

## What this book is

Two halves — both committed to the repo, the second half regenerated from source on every commit:

- **Manual** — curated narrative. Architecture, the operator cookbook, contracts overview, troubleshooting. Hand-written; touched as rarely as the design changes.
- **Reference** — auto-generated. Every Rust module's `//!` and `///` comments + every `.nix` library file's `# `-comments, mirrored as Markdown by `nix run .#docs`. CI gates on no-drift via `nix run .#docs-check`.

If you're reading this because you're new to the codebase: read the [architecture](architecture.md) page first, then the [protocol overview](protocol-overview.md), then dip into the reference for whichever piece you're touching.

## Generating

```sh
# Regenerate everything in src/generated/. Idempotent — running twice
# against the same code produces byte-identical output.
nix run .#docs

# Build the rendered HTML book into ./book/.
mdbook build docs/mdbook
```

CI fails the build if `nix run .#docs-check` finds drift between committed sources and what a fresh extraction would produce. Update + commit the regenerated files alongside any code change that touches comments.

## Manual content lives next to me

Files in this directory (`src/manual/`) are **never** touched by the regeneration script. Edit them like normal Markdown — `nix run .#docs` only updates `src/generated/` and rewrites `src/SUMMARY.md`.
