# nixfleet documentation

Map of what lives where. Every doc here is authoritative for its topic; when
the code disagrees, the code is being built to match.

## Design + contracts (read first)

| File | What it is | When to read |
|---|---|---|
| [`../ARCHITECTURE.md`](../ARCHITECTURE.md) | High-level architecture, component roles, trust flow, build order (Phases 0–10) | First read for new contributors |
| [`CONTRACTS.md`](CONTRACTS.md) | Every artifact, key, and format that crosses a stream boundary (data, trust roots, canonicalization, storage purity) | When adding or changing anything cross-stream |
| [`KICKOFF.md`](KICKOFF.md) | v0.2 cycle flow, stream prompts (A / B / C), checkpoints, merge discipline | When starting work on a v0.2 stream |

## Protocol + data (RFCs)

| File | Topic |
|---|---|
| [`../rfcs/0001-fleet-nix.md`](../rfcs/0001-fleet-nix.md) | Declarative fleet shape: `mkFleet`, selectors, rollouts, edges, budgets, `fleet.resolved` artifact |
| [`../rfcs/0002-reconciler.md`](../rfcs/0002-reconciler.md) | Reconciler state machine, decision procedure, verify path, failure handling |
| [`../rfcs/0003-protocol.md`](../rfcs/0003-protocol.md) | Agent ↔ control-plane wire protocol, identity model, endpoints, security model |

## Operational reference (mdbook)

The [`mdbook/`](mdbook/) subtree is the user-facing manual built with mdbook.
Start at [`mdbook/README.md`](mdbook/README.md); the full table of contents
lives in [`mdbook/SUMMARY.md`](mdbook/SUMMARY.md). Notable sections:

| Section | Path |
|---|---|
| Architecture overview | [`mdbook/architecture.md`](mdbook/architecture.md) |
| Getting started (quick start, installation, design guarantees) | [`mdbook/guide/getting-started/`](mdbook/guide/getting-started/) |
| Defining hosts (`mkHost`, hostSpec, cross-platform) | [`mdbook/guide/defining-hosts/`](mdbook/guide/defining-hosts/) |
| Deploying (control plane, agent, cache, rollouts) | [`mdbook/guide/deploying/`](mdbook/guide/deploying/) |
| Operating (status, rollback, impermanence) | [`mdbook/guide/operating/`](mdbook/guide/operating/) |
| Extending (custom scopes, secrets, templates) | [`mdbook/guide/extending/`](mdbook/guide/extending/) |
| Reference (CLI, options, modules) | [`mdbook/reference/`](mdbook/reference/) |
| Testing (overview, eval, VM, Rust) | [`mdbook/testing/`](mdbook/testing/) |

## Historical decisions

| File | What it is |
|---|---|
| [`adr/`](adr/) | Architecture Decision Records (011 ADRs, numbered 001–011) covering `mkHost`, flags-over-roles, agent-as-service-module, hydration, fire-and-forget apply, etc. |
| [`superpowers/`](superpowers/) | Spec + plan artifacts from implementation cycles (`specs/`, `plans/`) |

## Root-level docs

| File | What it is |
|---|---|
| [`../README.md`](../README.md) | User-facing README: install, quick start, ecosystem |
| [`../CHANGELOG.md`](../CHANGELOG.md) | Changelog (Keep a Changelog format) |
| [`../CONTRIBUTING.md`](../CONTRIBUTING.md) | Contributor guide: setup, tests, commit conventions, license |
| [`../SECURITY.md`](../SECURITY.md) | Security policy and disclosure |
| [`../CODE_OF_CONDUCT.md`](../CODE_OF_CONDUCT.md) | Code of conduct |
