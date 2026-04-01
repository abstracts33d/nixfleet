# ADR-002: hostSpec Flags over Roles and Presets

**Date:** 2026-03-31
**Status:** Accepted
**Spec:** `superpowers/specs/2026-03-31-nixfleet-simplification-design.md`

## Context

nixfleet had a role system (`mkRole`) defining named presets: workstation, server, edge, minimal, darwin-workstation. Each role bundled hostSpec defaults (e.g., workstation set `isGraphical = true`, `isDev = true`). Three options were considered: keep roles as a mkHost parameter, replace with composable preset modules, or drop roles entirely and use flags.

## Decision

Drop roles and presets. Each host sets its own hostSpec flags directly:

```nix
hostSpec = org // {
  isGraphical = true;
  isImpermanent = true;
  isDev = true;
};
```

Scopes already react to these flags via `lib.mkIf` — that machinery doesn't change.

## Alternatives Considered

1. **Roles as mkHost parameter** — `mkHost { role = "workstation"; ... }` applies role defaults. Rejected because roles add a concept that doesn't earn its weight. A "workstation" is just 3-4 flags.
2. **Composable preset modules** — `modules = [ nixfleet.presets.graphical nixfleet.presets.impermanent ]`. Each preset sets flags + optional config. Rejected because scopes already provide the composition layer — presets would be a redundant indirection that just sets flags that trigger scopes.

## Consequences

- Host definitions are fully self-describing — every capability is visible as a flag
- No "what does the workstation role include?" questions
- Repetition across similar hosts mitigated by shared `let` bindings (plain Nix)
- Zero framework concepts beyond "hostSpec flags control what's enabled"
- If repetition becomes a problem at scale, presets or mkFleetFlake can be added later as convenience
