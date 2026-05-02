{inputs, ...}: {
  perSystem = {
    pkgs,
    lib,
    config,
    ...
  }: let
    craneLib = inputs.crane.mkLib pkgs;
    workspace = import ../crane-workspace.nix {inherit lib craneLib;};
  in {
    inherit (workspace) checks;
    packages =
      workspace.packages
      // {
        # Fully-built static site: mdbook (curated guide + RFCs +
        # nixosOptionsDoc) with the cargo doc Rust API reference
        # mounted at `api/`. Pure derivation, no source-tree mutation —
        # consumers reference it as a /nix/store path (e.g. as a
        # reverse-proxy doc root or a CI publish target). Mirrors
        # what `apps.docs` writes into the working tree, but produces
        # a single immutable output.
        docs-site =
          pkgs.runCommand "nixfleet-docs-site" {
            nativeBuildInputs = [pkgs.mdbook];
          } ''
            cp -r ${inputs.self} src
            chmod -R u+w src
            cd src

            cp ${config.packages.options-doc} docs/mdbook/src/options.md

            mkdir -p docs/mdbook/src/rfcs
            cp docs/rfcs/*.md docs/mdbook/src/rfcs/

            mdbook build docs/mdbook

            mkdir -p docs/mdbook/book/api
            if [ -d ${workspace.cargoDocs}/share/doc ]; then
              cp -r ${workspace.cargoDocs}/share/doc/. docs/mdbook/book/api/
            else
              cp -r ${workspace.cargoDocs}/. docs/mdbook/book/api/
            fi

            cp -r docs/mdbook/book $out
          '';
      };

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

    # NB: there is no `apps.nixfleet` — `nixfleet-cli` ships
    # `nixfleet-mint-token` + `nixfleet-derive-pubkey` only. The
    # historical `apps.nixfleet` pointed at a binary that never
    # existed (left over from an early-design CLI plan), causing
    # `nix run .#nixfleet` to fail with "No such file or directory".
    # Operators reach the helpers via `nix shell nixfleet#nixfleet-cli`
    # (gets both binaries on PATH); the validate app + per-binary
    # apps below cover the rest.

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

    apps.nixfleet-release = {
      type = "app";
      program = "${workspace.packages.nixfleet-release}/bin/nixfleet-release";
      meta.description = "Producer for fleet.resolved.json — build → inject closureHash → canonicalize → sign → release (CONTRACTS §I #1)";
    };

    # Single doc build path: `packages.docs-site` (pure derivation,
    # above). Use `nix build .#packages.<system>.docs-site` to
    # produce the static site at `<store>/`. The previous procedural
    # `apps.docs` shell pipeline was redundant — it wrote the same
    # mdbook + cargo doc output into the working tree, with the only
    # operational difference being out-of-sandbox `cargo doc` reuse
    # of the local `target/doc` cache. Use the derivation for CI /
    # publishing; use `cargo doc` directly for dev-loop iteration.

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
