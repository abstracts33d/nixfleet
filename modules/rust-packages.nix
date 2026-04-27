{inputs, ...}: {
  perSystem = {
    pkgs,
    lib,
    ...
  }: let
    craneLib = inputs.crane.mkLib pkgs;
    workspace = import ../crane-workspace.nix {inherit lib craneLib;};
  in {
    inherit (workspace) packages checks;

    apps.agent = {
      type = "app";
      program = "${workspace.packages.nixfleet-agent}/bin/nixfleet-agent";
      meta.description = "NixFleet fleet management agent";
    };

    apps.control-plane = {
      type = "app";
      program = "${workspace.packages.nixfleet-control-plane}/bin/nixfleet-control-plane";
      meta.description = "NixFleet control plane server";
    };

    apps.nixfleet = {
      type = "app";
      program = "${workspace.packages.nixfleet-cli}/bin/nixfleet";
      meta.description = "NixFleet fleet management CLI";
    };

    apps.nixfleet-canonicalize = {
      type = "app";
      program = "${workspace.packages.nixfleet-canonicalize}/bin/nixfleet-canonicalize";
      meta.description = "JCS canonicalizer — invoked by CI before signing (CONTRACTS.md §III)";
    };

    apps.nixfleet-verify-artifact = {
      type = "app";
      program = "${workspace.packages.nixfleet-verify-artifact}/bin/nixfleet-verify-artifact";
      meta.description = "Phase 2 harness CLI — verify a signed fleet.resolved against a trust.json";
    };

    # Doc generator + drift check + full pipeline. The pipeline is a
    # tiny shell script that wipes the generated tree, runs each
    # extractor, and finally rebuilds SUMMARY.md. Determinism comes
    # from the docgen binary itself (sorted walks, no timestamps);
    # the wrapper just orchestrates the three subcommands.
    apps.docgen = {
      type = "app";
      program = "${workspace.packages.nixfleet-docgen}/bin/nixfleet-docgen";
      meta.description = "Markdown extractor for Rust + Nix sources (use `nix run .#docs` for the full pipeline)";
    };

    apps.docs = {
      type = "app";
      program = let
        script = pkgs.writeShellApplication {
          name = "nixfleet-docs";
          runtimeInputs = [workspace.packages.nixfleet-docgen pkgs.coreutils];
          text = ''
            set -euo pipefail
            repo_root="''${1:-$PWD}"
            book_src="$repo_root/docs/mdbook/src"
            generated="$book_src/generated"
            echo "regenerating docs into $generated"
            mkdir -p "$generated"
            nixfleet-docgen rust "$repo_root" "$generated"
            nixfleet-docgen nix-comments "$repo_root" "$generated"
            # Copy RFCs verbatim so the book ships them alongside the
            # auto-generated reference. They're committed Markdown
            # already; copying preserves the byte-identical guarantee.
            mkdir -p "$generated/rfcs"
            for f in "$repo_root"/rfcs/*.md; do
              [ -f "$f" ] || continue
              cp "$f" "$generated/rfcs/$(basename "$f")"
            done
            nixfleet-docgen summary "$book_src"
            echo "done"
          '';
        };
      in "${script}/bin/nixfleet-docs";
      meta.description = "Regenerate all auto-extracted docs in docs/mdbook/src/generated/";
    };

    apps.docs-check = {
      type = "app";
      program = let
        script = pkgs.writeShellApplication {
          name = "nixfleet-docs-check";
          runtimeInputs = [workspace.packages.nixfleet-docgen pkgs.coreutils pkgs.diffutils pkgs.git];
          text = ''
            set -euo pipefail
            repo_root="''${1:-$PWD}"
            tmp=$(mktemp -d)
            trap 'rm -rf "$tmp"' EXIT
            cp -r "$repo_root/docs/mdbook/src" "$tmp/expected"

            book_src="$repo_root/docs/mdbook/src"
            generated="$book_src/generated"
            mkdir -p "$generated"
            nixfleet-docgen rust "$repo_root" "$generated"
            nixfleet-docgen nix-comments "$repo_root" "$generated"
            mkdir -p "$generated/rfcs"
            for f in "$repo_root"/rfcs/*.md; do
              [ -f "$f" ] || continue
              cp "$f" "$generated/rfcs/$(basename "$f")"
            done
            nixfleet-docgen summary "$book_src"

            if ! diff -ru "$tmp/expected" "$repo_root/docs/mdbook/src" > "$tmp/diff" 2>&1; then
              echo "doc drift detected — committed docs are stale" >&2
              cat "$tmp/diff" >&2
              # Restore the committed tree so a failed check doesn't
              # leave the working copy modified.
              rm -rf "$repo_root/docs/mdbook/src"
              cp -r "$tmp/expected" "$repo_root/docs/mdbook/src"
              exit 1
            fi
            echo "docs in sync with sources"
          '';
        };
      in "${script}/bin/nixfleet-docs-check";
      meta.description = "Fail with a diff when committed docs/mdbook/src/generated/ is out of sync with the sources";
    };

    devShells.default = craneLib.devShell {
      checks = workspace.checks;
      packages = with pkgs; [
        cargo-nextest
        rust-analyzer
        git
        age
        bashInteractive
      ];
      shellHook = ''
        export EDITOR=vim
        git config core.hooksPath .githooks 2>/dev/null || true
      '';
    };
  };
}
