# Project Rules

## Purpose

8 rule files in `.claude/rules/` providing domain-specific guidance. These are project-level rules (git-tracked) that apply to all Claude sessions in this repo.

## Location

- `.claude/rules/`

## Rule Table

| Rule | File | Domain |
|------|------|--------|
| Config Dependencies | `config-dependencies.md` | What to update when changing linked configs |
| Git Workflow | `git-workflow.md` | Branch strategy, PR workflow, shipping convention |
| Multi-Repo | `multi-repo.md` | Multi-repo coordination (fleet, nixfleet, secrets) |
| Nix Style | `nix-style.md` | Code formatting and conventions |
| Platform Design | `platform-design.md` | Cross-platform compatibility guards |
| Security Review | `security-review.md` | Security audit process and findings |
| Testing | `testing.md` | Test pyramid and how to add tests |
| Wrapper Boundary | `wrapper-boundary.md` | What goes in wrappers vs HM vs scopes |

## Key Rules Summary

**Config Dependencies:** Bidirectional dependency chains between `_config/`, `core/_home/`, `wrappers/`, and `_shared/`. Change one, check the other.

**Git Workflow:** Feature branches required, PRs for all changes to main, squash-merge only. Never push directly to main or merge PRs automatically.

**Multi-Repo:** Three repos (nixfleet, fleet, fleet-secrets). Secrets must be committed in fleet-secrets first, then `nix flake update secrets`.

**Nix Style:** Format with alejandra, use `lib.mkIf`/`lib.mkDefault`, no `with pkgs;` in module-level let bindings.

**Platform Design:** Guards for Darwin, impermanence, network interfaces. Don't over-engineer cross-platform — note ambitious ideas as TODOs.

**Security Review:** Monthly audits with timestamped reports. 3-level permissions model (org deny, project allow, user mode).

**Testing:** Eval tests for config correctness, VM tests for runtime behavior. Add both when creating new scopes.

**Wrapper Boundary:** Individual tools -> HM. Portable composites -> wrappers. GPU-dependent -> NixOS scopes.

## Links

- [Claude Overview](README.md)
- [Scopes](scopes.md) (where rules are delivered)
