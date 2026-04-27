//! `nixfleet-docgen` — extract Markdown reference docs from nixfleet's
//! Rust + Nix sources.
//!
//! Three subcommands:
//!
//! - `rust <repo-root> <out-dir>`: walk `<repo-root>/crates/*/src/`,
//!   parse each `.rs` with `syn`, emit one `.md` per source file
//!   into `<out-dir>/rust/<crate>/<path>.md`. Includes ALL items
//!   (public + private). Skips `#[cfg(test)]` modules.
//!
//! - `nix-comments <repo-root> <out-dir>`: walk a curated list of
//!   `.nix` files (lib/ + modules/scopes/nixfleet/), extract leading
//!   file-level comments + per-binding `# `-comments, emit `.md`.
//!
//! - `summary <book-src-dir>`: walk `<book-src-dir>/manual/` and
//!   `<book-src-dir>/generated/`, emit a deterministic `SUMMARY.md`.
//!
//! Determinism: every directory walk is sorted, every output file is
//! written atomically, no timestamps appear in the rendered Markdown.
//! Running twice against the same source tree produces byte-identical
//! files. CI runs `docs-check` (regenerate to a tmpdir + diff against
//! committed); a non-empty diff fails the build.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

mod nix_comments;
mod rust_extractor;
mod summary;

#[derive(Parser, Debug)]
#[command(name = "nixfleet-docgen", version, about = "Generate Markdown docs from Rust + Nix sources.")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Walk crates/*/src/, extract Rust doc comments, emit .md.
    Rust {
        /// Repo root (contains `crates/`).
        repo_root: PathBuf,
        /// Output directory for generated `.md` files.
        out_dir: PathBuf,
    },
    /// Walk a curated set of .nix files, extract comments, emit .md.
    NixComments {
        /// Repo root (contains `lib/` and `modules/`).
        repo_root: PathBuf,
        /// Output directory for generated `.md` files.
        out_dir: PathBuf,
    },
    /// Walk an mdbook src/ tree, emit deterministic SUMMARY.md.
    Summary {
        /// mdbook src/ directory (contains manual/ and generated/).
        book_src: PathBuf,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Rust { repo_root, out_dir } => {
            ensure_clean_dir(&out_dir.join("rust"))?;
            rust_extractor::run(&repo_root, &out_dir.join("rust"))
        }
        Cmd::NixComments { repo_root, out_dir } => {
            ensure_clean_dir(&out_dir.join("nix"))?;
            nix_comments::run(&repo_root, &out_dir.join("nix"))
        }
        Cmd::Summary { book_src } => summary::run(&book_src),
    }
}

/// Ensure `dir` exists and is empty. Used by each subcommand before
/// emitting fresh files so stale output from a prior run can never
/// silently survive a rename/delete in the source tree.
fn ensure_clean_dir(dir: &Path) -> Result<()> {
    if dir.exists() {
        fs::remove_dir_all(dir)
            .with_context(|| format!("remove stale {}", dir.display()))?;
    }
    fs::create_dir_all(dir)
        .with_context(|| format!("create {}", dir.display()))?;
    Ok(())
}
