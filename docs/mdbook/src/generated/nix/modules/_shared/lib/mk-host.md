# `modules/_shared/lib/mk-host.nix`

mkHost - the single NixFleet API function.
Returns a nixosSystem or darwinSystem with framework-level mechanism only.

Opinions (base CLI tools, firewall, secrets, backup, monitoring,
impermanence, home-manager, disko) live in `arcanesys/nixfleet-scopes`.
Consumers compose them via the `modules` argument:

    mkHost {
      hostName = "myhost"; platform = "x86_64-linux";
      hostSpec = { userName = "alice"; };
      modules = [
        inputs.nixfleet-scopes.scopes.roles.workstation  # WHAT it is
        ./modules/profiles/developer.nix                  # WHO uses it / HOW
        ./modules/hardware/desktop-amd-nvidia.nix         # WHAT hardware
        ./modules/hosts/myhost/hardware-configuration.nix
      ];
    };

Per Decision 2 + 3 of the scopes-extraction plan (rev 4):
- Home Manager is a scope (not a framework service); consumers pull it
  in via `nixfleet-scopes.scopes.home-manager` (usually indirectly
  through a role) and add their own user-level HM imports.
- disko + impermanence are scopes too; mkHost does not auto-import
  their NixOS modules any more.

## Bindings

### `coreNixos`

Core modules (plain NixOS/Darwin modules)

### `agentModule`

Service modules (auto-included, disabled by default)

### `frameworkDarwinModules`

Framework Darwin modules injected by mkHost.

The operator scope is platform-agnostic (just systemPackages +
an env var) so it gets the same module file as NixOS. Same
`enable=false` default means Darwin hosts that don't need
operator tooling aren't impacted; aether enables it via fleet
wiring (see fleet/modules/nixfleet/operator.nix).

### `buildNixos`

Build NixOS system. Framework inputs passed via specialArgs so
consumer-imported modules (including nixfleet-scopes scopes) can
reach inputs.home-manager, inputs.disko, inputs.impermanence, …

### `buildDarwin`

Build Darwin system. stateVersion is Darwin-specific (integer);
consumers set `system.stateVersion` in their host modules.

