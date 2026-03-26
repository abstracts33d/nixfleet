# Adding a New Scope

How to add a new feature group that self-activates based on host flags.

## 1. Define the Flag

Add a new option in `modules/_shared/host-spec-module.nix`:

```nix
useMyFeature = lib.mkOption {
  type = lib.types.bool;
  default = false;
  description = "Enable my feature";
};
```

## 2. Create the Scope Module

Add a file in `modules/scopes/` — import-tree picks it up automatically:

```nix
# modules/scopes/my-feature.nix
{config, lib, pkgs, ...}: let
  hS = config.hostSpec;
in {
  config = lib.mkIf hS.useMyFeature {
    environment.systemPackages = with pkgs; [ ... ];
    # services, config, etc.
  };
}
```

## 3. Add Home Manager Config (if needed)

For user-level configuration, add a deferred HM module:

```nix
config.flake.modules.homeManager.my-feature = {config, lib, ...}: let
  hS = config.hostSpec;
in {
  config = lib.mkIf hS.useMyFeature {
    programs.something.enable = true;
  };
};
```

## 4. Add Persist Paths (if needed)

If the feature stores state, add persist paths in the same module:

```nix
home.persistence."/persist" = lib.optionalAttrs (!hS.isDarwin) {
  directories = [ ".local/share/my-feature" ];
};
```

## 5. Add Tests

- **Eval test** in `modules/tests/eval.nix` — verify the scope activates/deactivates
- **VM test** in `modules/tests/vm.nix` — verify runtime behavior (if applicable)

## 6. Update Docs

Update CLAUDE.md (flags table, module tree) and README.md (scopes table).

## Further Reading

- [The Scope System](../concepts/scopes.md) — conceptual overview
- [Technical Scope Details](../../src/scopes/README.md) — all scope modules
