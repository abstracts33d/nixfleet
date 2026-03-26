# Project Rules

## Purpose

8 rule files in `.claude/rules/` providing domain-specific guidance. These are project-level rules (git-tracked) that apply to all Claude sessions in this repo.

## Location

- `.claude/rules/`

## Rule Table

| Rule | File | Domain |
|------|------|--------|
| Config Dependencies | `config-dependencies.md` | What to update when changing linked configs |
| Nix Gotchas | `nix-gotchas.md` | 12 pitfalls learned in this repo |
| Nix Style | `nix-style.md` | Code formatting and conventions |
| Platform Design | `platform-design.md` | Cross-platform compatibility guards |
| Wrapper Boundary | `wrapper-boundary.md` | What goes in wrappers vs HM vs scopes |
| Security Review | `security-review.md` | Security audit process and findings |
| Testing | `testing.md` | Test pyramid and how to add tests |
| Multi-Repo | `multi-repo.md` | Multi-repo coordination |

## Key Rules Summary

**Config Dependencies:** Bidirectional dependency chains between `_config/`, `core/_home/`, `wrappers/`, and `_shared/`. Change one, check the other.

**Nix Gotchas:** `perSystem` pkgs don't inherit `allowUnfree`, catppuccin has no darwinModules, don't persist `.ssh`/`.gnupg`, `home.persistence` needs `optionalAttrs` on Darwin.

**Wrapper Boundary:** Individual tools -> HM. Portable composites -> wrappers. GPU-dependent -> NixOS scopes.

**Testing:** Eval tests for config correctness, VM tests for runtime behavior. Add both when creating new scopes.

## Links

- [Claude Overview](README.md)
- [Scopes](scopes.md) (where rules are delivered)
