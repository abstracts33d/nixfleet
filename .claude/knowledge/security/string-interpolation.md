# String Interpolation Safety in NixOS Modules

## Rule

Never interpolate NixOS option values directly into shell strings without escaping.
User-supplied option values can contain spaces, quotes, or special characters that break
commands or enable injection.

## Safe patterns

- `lib.escapeShellArg value` — for a single shell argument (wraps in single quotes, escapes embedded quotes)
- `lib.escapeShellArgs [list]` — for a list of arguments (applies escapeShellArg to each)
- `toString intValue` — for integers (safe: Nix int → decimal string, no special chars)
- `${pkg}/bin/name` — for store paths (safe: Nix store paths are deterministic and ASCII-clean)
- Values assigned to NixOS option attributes (not shell strings) — safe, no escaping needed

## Unsafe patterns

```nix
# BAD — spaces or quotes in cfg.userInput break the command
ExecStart = "... --flag ${cfg.userInput}";

# BAD — cfg.secretPath could be a set (mkDefault) instead of a plain string
ExecStart = "... ${cfg.secretPath}";

# BAD — list element interpolated raw into bash heredoc/concatStringsSep
ExecStart = lib.concatStringsSep " " ["--url" cfg.controlPlaneUrl];
```

## Correct patterns

```nix
# GOOD — single string argument escaped
ExecStart = lib.concatStringsSep " " [
  "${pkg}/bin/cmd"
  "--url" (lib.escapeShellArg cfg.controlPlaneUrl)
  "--count" (toString cfg.count)
];

# GOOD — list of arguments escaped in bulk
ExecStart = lib.concatStringsSep " " (
  ["${pkg}/bin/cmd"]
  ++ lib.escapeShellArgs cfg.extraArgs
);
```

## Nix-time vs runtime interpolation

Nix evaluates `${expr}` at build time. When `expr` is a controlled Nix value (e.g., a
`wifiNetworks` list element iterated with `map`), the result is baked into the script as a
literal — no runtime injection risk. Shell escaping is still recommended for clarity and
defence-in-depth, but is not strictly required for values that are never user-supplied at
runtime.

When `expr` is a NixOS option that end-users configure (e.g., `controlPlaneUrl`,
`machineId`, `dbPath`), always escape — the value is effectively user input.

## Files fixed in this repo

- `modules/scopes/nixfleet/agent.nix` — ExecStart args (`controlPlaneUrl`, `machineId`,
  `dbPath`, `cacheUrl`) wrapped with `lib.escapeShellArg`.
