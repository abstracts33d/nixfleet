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
      meta.description = "Harness CLI — verify a signed fleet.resolved against a trust.json";
    };

    # Doc pipeline using STANDARD tooling — `cargo doc` for the Rust
    # API reference, `nixosOptionsDoc` for the NixOS option reference
    # (see options-doc.nix), `mdbook build` for the curated narrative
    # + RFCs. Each tool produces what it natively renders best.
    apps.docs = {
      type = "app";
      program = let
        script = pkgs.writeShellApplication {
          name = "nixfleet-docs";
          runtimeInputs = [pkgs.cargo pkgs.rustc pkgs.coreutils pkgs.mdbook pkgs.nix];
          text = ''
            set -euo pipefail
            repo_root="''${1:-$PWD}"

            echo "==> cargo doc --workspace --document-private-items --no-deps"
            (cd "$repo_root" && \
              RUSTDOCFLAGS="-D rustdoc::broken-intra-doc-links" \
              cargo doc --workspace --document-private-items --no-deps)

            echo "==> nix build .#options-doc (nixosOptionsDoc → markdown)"
            options_md=$(cd "$repo_root" && nix build --no-link --print-out-paths .#options-doc)
            cp -f "$options_md" "$repo_root/docs/mdbook/src/options.md"
            chmod u+w "$repo_root/docs/mdbook/src/options.md"

            echo "==> copying RFCs into mdbook"
            mkdir -p "$repo_root/docs/mdbook/src/rfcs"
            for f in "$repo_root"/rfcs/*.md; do
              [ -f "$f" ] || continue
              cp -f "$f" "$repo_root/docs/mdbook/src/rfcs/$(basename "$f")"
            done
            chmod -R u+w "$repo_root/docs/mdbook/src/rfcs/"

            echo "==> mdbook build docs/mdbook"
            (cd "$repo_root" && mdbook build docs/mdbook)

            echo "==> copying cargo doc output into the published site"
            mkdir -p "$repo_root/docs/mdbook/book/api"
            cp -r "$repo_root/target/doc/." "$repo_root/docs/mdbook/book/api/"

            echo
            echo "Done. Outputs:"
            echo "  - docs/mdbook/book/         (mdbook: curated guides + RFCs + options)"
            echo "  - docs/mdbook/book/api/     (cargo doc: Rust API reference)"
          '';
        };
      in "${script}/bin/nixfleet-docs";
      meta.description = "Build docs: cargo doc + nixosOptionsDoc + mdbook";
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
