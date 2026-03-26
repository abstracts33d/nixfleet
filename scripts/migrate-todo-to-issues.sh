#!/usr/bin/env bash
# scripts/migrate-todo-to-issues.sh
# Migrates TODO.md items to GitHub Issues.
# Run once: bash scripts/migrate-todo-to-issues.sh
# Skips items marked DONE in TODO.md.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/gh-issue-helper.sh"

echo "==> Migrating TODO.md to GitHub Issues..."
echo "    Repo: $REPO"
echo ""

# ---------------------------------------------------------------------------
# HIGH PRIORITY
# ---------------------------------------------------------------------------

echo "[1/25] NixFleet: Paradigm Shift (epic)"
gh_create_issue \
  "NixFleet: Paradigm Shift — Transform nixos-config into a fleet management platform" \
  '## Context

Epic tracking the full NixFleet roadmap. This config is the reference implementation.

Spec: `docs/superpowers/specs/2026-03-25-nixfleet-paradigm-shift.md`
Business docs: `docs/nixfleet/`

## Phases

- [ ] **S1: Multi-Org Hosts** — Generalize architecture for multiple organizations
- [ ] **S2: Role-Based Config** — Role assignments per host (workstation, server, kiosk)
- [ ] **S3: Fleet Agent** — Rust agent for remote apply, health reporting
- [ ] **S4: Control Plane** — Extend Go dashboard with fleet-wide management UI
- [ ] **S5: Binary Cache** — Attic-based shared binary cache per org
- [ ] **S6: Air-Gap Deploy** — Offline / air-gapped deployment support
- [ ] **S7: NIS2 Compliance** — EU compliance tooling and audit trails
- [ ] **S8: Open-Core** — Public release, licensing, open-core SaaS model

## Deliverables

- [ ] Each phase above tracked as its own milestone with child issues
- [ ] Architecture docs updated as each phase lands
- [ ] README.md NixFleet section kept in sync' \
  "scope:nixfleet,feature,impact:critical,urgency:soon"

echo "[2/25] Enterprise: VPN"
gh_create_issue \
  "Enterprise: Implement VPN scope (WireGuard / OpenVPN)" \
  '## Context

Stub exists at `modules/scopes/enterprise/vpn.nix`.
hostSpec flag: `useVpn`
Spec: `docs/superpowers/specs/2026-03-25-enterprise-features.md`

## Deliverables

- [ ] Implement `modules/scopes/enterprise/vpn.nix` with WireGuard support
- [ ] Support OpenVPN as alternative backend
- [ ] Secrets managed via agenix (WireGuard private key, PSK)
- [ ] Per-host config via `hostSpec.vpnConfig` or nix-secrets mapping
- [ ] Eval test: `useVpn = true` activates the scope
- [ ] README.md enterprise scopes table updated' \
  "scope:enterprise,feature,impact:high,urgency:soon,phase:S2" \
  "S2: Role-Based Config"

echo "[3/25] Enterprise: File sharing"
gh_create_issue \
  "Enterprise: Implement file sharing scope (Samba / CIFS)" \
  '## Context

Stub exists at `modules/scopes/enterprise/filesharing.nix`.
hostSpec flag: `useFilesharing`
Spec: `docs/superpowers/specs/2026-03-25-enterprise-features.md`

## Deliverables

- [ ] Implement Samba server config for workgroup/AD environments
- [ ] CIFS automount for network drives via `fileSystems`
- [ ] Credentials via agenix (samba password, domain join token)
- [ ] Eval test: `useFilesharing = true` activates the scope
- [ ] README.md enterprise scopes table updated' \
  "scope:enterprise,feature,impact:high,urgency:soon,phase:S2" \
  "S2: Role-Based Config"

echo "[4/25] Enterprise: LDAP/AD authentication"
gh_create_issue \
  "Enterprise: Implement LDAP/AD authentication scope (sssd / PAM)" \
  '## Context

Stub exists at `modules/scopes/enterprise/auth.nix`.
hostSpec flag: `useLdap`
Spec: `docs/superpowers/specs/2026-03-25-enterprise-features.md`

## Deliverables

- [ ] Implement sssd config for LDAP and Active Directory backends
- [ ] PAM integration for login (GDM, greetd, SSH)
- [ ] Kerberos ticket management
- [ ] Domain join via agenix-managed credentials
- [ ] Eval test: `useLdap = true` activates the scope
- [ ] README.md enterprise scopes table updated' \
  "scope:enterprise,feature,impact:high,urgency:soon,phase:S2" \
  "S2: Role-Based Config"

echo "[5/25] Enterprise: Network printing"
gh_create_issue \
  "Enterprise: Implement network printing scope (CUPS + auto-discovery)" \
  '## Context

Stub exists at `modules/scopes/enterprise/printing.nix`.
hostSpec flag: `usePrinting`
Spec: `docs/superpowers/specs/2026-03-25-enterprise-features.md`

## Deliverables

- [ ] Implement CUPS with Avahi/mDNS auto-discovery
- [ ] Common printer drivers (HP, Epson, Brother, generic PostScript)
- [ ] IPP Everywhere / AirPrint support
- [ ] Eval test: `usePrinting = true` activates the scope
- [ ] README.md enterprise scopes table updated' \
  "scope:enterprise,feature,impact:high,urgency:soon,phase:S2" \
  "S2: Role-Based Config"

echo "[6/25] Enterprise: Corporate certificates"
gh_create_issue \
  "Enterprise: Implement corporate CA certificate trust scope" \
  '## Context

Stub exists at `modules/scopes/enterprise/certificates.nix`.
hostSpec flag: `useCorporateCerts`
Spec: `docs/superpowers/specs/2026-03-25-enterprise-features.md`

## Deliverables

- [ ] Install corporate CA certs into system trust store (`security.pki.certificates`)
- [ ] Deploy client certificates via agenix to expected paths
- [ ] Browser trust integration (NSS database for Firefox/Chrome)
- [ ] Eval test: `useCorporateCerts = true` activates the scope
- [ ] README.md enterprise scopes table updated' \
  "scope:enterprise,feature,impact:high,urgency:soon,phase:S2" \
  "S2: Role-Based Config"

echo "[7/25] Enterprise: System proxy"
gh_create_issue \
  "Enterprise: Implement system-wide HTTP/HTTPS proxy scope" \
  '## Context

Stub exists at `modules/scopes/enterprise/proxy.nix`.
hostSpec flag: `useProxy`
Spec: `docs/superpowers/specs/2026-03-25-enterprise-features.md`

## Deliverables

- [ ] Set system-wide `http_proxy`/`https_proxy`/`no_proxy` environment variables
- [ ] Nix daemon proxy config (`nix.settings.extra-substituters` compatible)
- [ ] Proxy config via nix-secrets (URL, credentials)
- [ ] Eval test: `useProxy = true` activates the scope
- [ ] README.md enterprise scopes table updated' \
  "scope:enterprise,feature,impact:high,urgency:soon,phase:S2" \
  "S2: Role-Based Config"

echo "[8/25] Share Claude Code project memories across machines"
gh_create_issue \
  "Share Claude Code project memories across machines" \
  '## Context

Project memories live in `~/.claude/projects/<path-encoded>/memory/`. The path encoding differs between machines (`-mnt-dev-nixos-config` vs `-Users-s33d-.local-share-src-nixos-config`), so memories are not shared.

## Problem

- Claude Code writes to these files at runtime — `home.file` would overwrite them
- Path encoding is not currently configurable upstream
- Symlinks to a shared location require knowing the encoded path per machine

## Deliverables

- [ ] Investigate whether Claude Code supports configurable memory paths
- [ ] Explore: symlink `.claude/projects/*/memory/` to a git-tracked dir in this repo
- [ ] Explore: `programs.claude-code.memory` option in HM module
- [ ] If feasible: implement chosen approach in `modules/scopes/dev/home.nix`
- [ ] If not feasible: open upstream issue / contribute configurable paths
- [ ] Document final approach in CLAUDE.md Memory Scopes section' \
  "scope:claude,feature,impact:medium,urgency:later,phase:S0" \
  "S0: Foundation"

echo "[9/25] Manage Claude Code plugins declaratively"
gh_create_issue \
  "Manage Claude Code plugins declaratively via Nix" \
  "## Context

Plugins (superpowers, code-simplifier) are enabled via \`programs.claude-code.settings.enabledPlugins\` but plugin data/cache in \`~/.claude/plugins/\` is runtime-managed and not reproducible across machines.

## Deliverables

- [ ] Audit what \`~/.claude/plugins/\` contains and what is safe to declare
- [ ] Extend \`modules/scopes/dev/home.nix\` with plugin source declarations
- [ ] Explore upstream HM module support for plugin management
- [ ] If upstream doesn't support it: add impermanence persist path for plugins dir
- [ ] Document approach in CLAUDE.md Automation Layer section" \
  "scope:claude,feature,impact:low,urgency:later,phase:S0" \
  "S0: Foundation"

echo "[10/25] Add nixos-hardware profiles for krach and ohm"
gh_create_issue \
  "Add nixos-hardware profiles for krach and ohm" \
  "## Context

\`inputs.nixos-hardware\` is already in \`flake.nix\`. Profiles need to be matched to actual hardware.

## Steps

1. Run on each machine:
   \`\`\`sh
   sudo dmidecode -s system-product-name
   sudo dmidecode -s system-manufacturer
   grep 'model name' /proc/cpuinfo | head -1
   lspci | grep -i vga
   ls /sys/class/power_supply/ 2>/dev/null
   lsblk -d -o NAME,ROTA
   \`\`\`
2. Match to profiles at https://github.com/NixOS/nixos-hardware/tree/master

## Deliverables

- [ ] Identify hardware on krach (likely common-cpu-amd + common-pc-ssd)
- [ ] Identify hardware on ohm (likely common-cpu-amd + common-pc-laptop)
- [ ] Add matching profiles to \`hardwareModules\` in \`modules/hosts/krach.nix\` and \`modules/hosts/ohm.nix\`
- [ ] Rebuild and verify: \`nix run .#build-switch\`
- [ ] Run eval tests: \`nix flake check\`" \
  "scope:hardware,infra,impact:medium,urgency:soon,phase:S0" \
  "S0: Foundation"

echo "[11/25] Test and refine Niri + Noctalia desktop"
gh_create_issue \
  "Test and refine Niri + Noctalia desktop on real hardware" \
  '## Context

Niri boots in VM (krach-qemu) with greetd — basic setup works. Real hardware testing and UX refinement are needed.

## Deliverables

- [ ] Deploy and test on krach (physical machine)
- [ ] Refine keybinds in niri config (`modules/scopes/desktop/niri.nix`)
- [ ] Dump noctalia state: `noctalia-shell ipc call state all > noctalia.json` and review
- [ ] Confirm pipewire audio works on real hardware
- [ ] Confirm font rendering is correct with catppuccin theme
- [ ] Document any hardware-specific tweaks needed
- [ ] Add VM test assertions for niri session startup (`modules/tests/vm.nix`)' \
  "scope:desktop,feature,impact:medium,urgency:soon,phase:S0" \
  "S0: Foundation"

echo "[12/25] Provision WiFi secrets via agenix"
gh_create_issue \
  "Provision WiFi secrets via agenix (complete wifiNetworks implementation)" \
  '## Context

`wifiNetworks` hostSpec option is implemented but no encrypted secrets exist yet in nix-secrets. Each entry maps to `wifi-<name>.age`.

## Steps

1. On each machine: `nmcli connection export <ssid> > wifi-<name>.nmconnection`
2. Encrypt: `age -R ~/.ssh/id_ed25519.pub wifi-<name>.nmconnection > wifi-<name>.age`
3. Add to nix-secrets repo and commit
4. Update flake input: `nix flake update secrets`

## Deliverables

- [ ] Export .nmconnection files for all required SSIDs (krach, ohm)
- [ ] Encrypt and commit to nix-secrets
- [ ] Update flake.lock: `nix flake update secrets`
- [ ] Verify WiFi bootstrap on fresh install (test with krach-qemu or similar)
- [ ] Add eval test asserting WiFi service activates when wifiNetworks is non-empty' \
  "scope:core,infra,impact:medium,urgency:soon,phase:S0" \
  "S0: Foundation"

# ---------------------------------------------------------------------------
# MEDIUM PRIORITY
# ---------------------------------------------------------------------------

echo "[13/25] Dashboard Tier 2 D3 visualizations"
gh_create_issue \
  "Dashboard: Add Tier 2 D3 visualizations (heatmap, chord, orbital, etc.)" \
  "## Context

The dashboard (\`dashboard/\`) already has Tier 1 views. These are the planned Tier 2 D3-powered visualizations.

## Deliverables

- [ ] **Heatmap Grid**: git change frequency per file/week (GitHub contributions style)
- [ ] **Ephemeral/Persistent Yin-Yang**: impermanence visualization with boot animation
- [ ] **Voronoi Coverage**: test coverage map with untested file 'deserts'
- [ ] **Chord Diagram**: config file dependency visualization (from config-dependencies.md)
- [ ] **Orbital System**: host closure sizes as planetary orbits
- [ ] **Particle Flow**: animated build/deploy pipeline visualization
- [ ] Add routes to \`dashboard/main.go\` and nav links to front index
- [ ] Update \`docs/src/apps/dashboard.md\` with new views" \
  "scope:nixfleet,feature,impact:medium,urgency:later,phase:S4" \
  "S4: Control Plane"

echo "[14/25] Scheduled automated tasks"
gh_create_issue \
  "Implement scheduled automated Claude Code tasks (Phase D)" \
  '## Context

Automated maintenance tasks to run on a schedule via Claude Code or server cron.

## Planned tasks

- Security audit: weekly via `/security` skill
- Flake freshness: weekly alert if `flake.lock` > 14 days old
- Config dependency drift: daily checksum comparison of linked files
- Build health: nightly `nix run .#validate`, alert on failure only

## Deliverables

- [ ] Evaluate Anthropic cloud scheduled tasks availability
- [ ] Alternatively: server cron + `claude --bare` on a Linux host
- [ ] Implement security audit schedule
- [ ] Implement flake freshness check
- [ ] Implement config dependency drift detector
- [ ] Implement nightly build health check
- [ ] Document automation in CLAUDE.md Automation Layer section' \
  "scope:claude,infra,impact:medium,urgency:later,phase:S0" \
  "S0: Foundation"

echo "[15/25] Agent teams for parallel refactoring"
gh_create_issue \
  "Enable agent teams for parallel multi-scope refactoring (Phase E)" \
  '## Context

Once `CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1` exits experimental, agent teams can be used for large refactoring tasks (one agent per host, shared task list).

## Deliverables

- [ ] Monitor Claude Code changelog for agent teams GA
- [ ] Design task decomposition for multi-scope refactoring
- [ ] Update `.claude/agents/` with team-aware configurations
- [ ] Write a `/refactor` skill using agent teams
- [ ] Test on a real multi-scope refactoring task (e.g., migrating impermanence to preservation)
- [ ] Update CLAUDE.md Automation Layer section' \
  "scope:claude,feature,impact:low,urgency:later,phase:S0" \
  "S0: Foundation"

echo "[16/25] Configure remote Linux builder for macOS"
gh_create_issue \
  "Configure remote Linux builder for macOS cross-compilation" \
  '## Context

macOS cannot build aarch64-linux natively. A remote builder (krach or any Linux machine) would enable fast cached builds for all `nix build` commands, not just installs.

## Options

- `nix.buildMachines` in Darwin config pointing to a Linux host via SSH
- `nix.linux-builder` (requires `nix.enable = true`, incompatible with Determinate installer)

## Deliverables

- [ ] Add `nix.buildMachines` block to `modules/core/darwin.nix`
- [ ] Set up SSH key trust between aether and krach
- [ ] Test: `nix build .#nixosConfigurations.krach.config.system.build.toplevel` from aether
- [ ] Document in README.md macOS section
- [ ] Consider when: when macOS VM workflow becomes frequent' \
  "scope:core,infra,impact:medium,urgency:later,phase:S0" \
  "S0: Foundation"

echo "[17/25] Replace UTM with Lima for macOS VMs"
gh_create_issue \
  "Replace UTM with Lima for macOS VM management" \
  '## Context

- UTM: sandboxed, AppleScript API unreliable, `utmctl` incomplete
- Lima: fully CLI, Apple Virtualization Framework, SSH/port-forwarding automatic
- Same UX as `spawn-qemu` on Linux

References:
- https://github.com/lima-vm/lima
- https://github.com/ciderale/nixos-lima

## Deliverables

- [ ] Add lima to nixpkgs deps in Darwin config or wrapper
- [ ] Write `spawn-vm` app that works on both Linux (QEMU) and macOS (Lima)
- [ ] Retire or gate `spawn-utm` app behind a flag
- [ ] Update README.md VM section with Lima instructions
- [ ] Update CLAUDE.md build commands section' \
  "scope:core,refactor,impact:low,urgency:later,phase:S0" \
  "S0: Foundation"

echo "[18/25] Fix xdg-desktop-portal config warning"
gh_create_issue \
  "Fix xdg-desktop-portal config warning (portal 1.17+)" \
  '## Context

Build warns: `xdg.portal.config should be set for portal 1.17+`

Fix: add `xdg.portal.config.common.default = "*";` or use `configPackages` in the graphical scope.

## Deliverables

- [ ] Add appropriate `xdg.portal.config` in `modules/scopes/graphical/`
- [ ] Verify warning disappears on next build
- [ ] Check if niri-specific portal backend should be preferred over wildcard' \
  "scope:desktop,bug,impact:low,urgency:later,phase:S0" \
  "S0: Foundation"

echo "[19/25] Migrate impermanence to preservation"
gh_create_issue \
  "Migrate impermanence to preservation (nix-community/preservation)" \
  '## Context

preservation is the modern replacement for impermanence. No runtime interpreters (more secure). Already adopted by top configs (ryan4yin).

Repo: https://github.com/nix-community/preservation

## Deliverables

- [ ] Add `inputs.preservation` to `flake.nix`
- [ ] Map current `home.persistence` entries to preservation equivalents
- [ ] Map current `environment.persistence` entries to preservation equivalents
- [ ] Update `modules/scopes/impermanence.nix` and all scope modules with persist paths
- [ ] Run VM tests to verify all persist paths still work: `nix run .#validate -- --vm`
- [ ] Remove impermanence input once migration is verified
- [ ] Update CLAUDE.md Key Integrations section' \
  "scope:core,refactor,impact:medium,urgency:later,phase:S0" \
  "S0: Foundation"

echo "[20/25] Add nixos-generators"
gh_create_issue \
  "Add nixos-generators for VM/ISO/cloud image generation" \
  '## Context

nixos-generators can produce ISO, QCOW2, Docker, AWS AMI images from NixOS config.
Repo: https://github.com/nix-community/nixos-generators

## Deliverables

- [ ] Add `inputs.nixos-generators` to `flake.nix`
- [ ] Create `modules/generators.nix` with image targets (iso, qcow2, docker)
- [ ] Wire into `packages.*` in flake outputs
- [ ] Consider retiring the manual `iso/` directory approach
- [ ] Update README.md build commands section
- [ ] Update CLAUDE.md Key Integrations section' \
  "scope:core,feature,impact:low,urgency:later,phase:S0" \
  "S0: Foundation"

echo "[21/25] Validation pipeline Phase 4-5"
gh_create_issue \
  "Validation pipeline: Phase 4 (GitHub Actions CI) and Phase 5 (advanced dependency checks)" \
  '## Context

Phases 1-3 are complete. Remaining phases:

**Phase 4 — GitHub Actions CI:**
- Run format check + full validation on PRs
- Cache nix store between runs (attic or cachix)

**Phase 5 — Advanced dependency checks:**
- Diff `wrapperrc.zsh` against `zsh.nix` programmatically
- Verify README.md hosts match `flake.nixosConfigurations`
- Verify CLAUDE.md flags table matches `host-spec-module.nix`

## Deliverables

- [ ] Write `.github/workflows/ci.yml` running nix fmt + nix run .#validate
- [ ] Configure Cachix or Attic for nix store caching in CI
- [ ] Write `scripts/check-zsh-drift.sh` comparing wrapperrc vs zsh.nix
- [ ] Write `scripts/check-readme-hosts.sh` comparing README vs flake outputs
- [ ] Write `scripts/check-claude-flags.sh` comparing CLAUDE.md vs host-spec-module.nix
- [ ] Wire advanced checks into `nix run .#validate` Phase 5 step' \
  "scope:testing,infra,impact:medium,urgency:later,phase:S0" \
  "S0: Foundation"

echo "[22/25] Testing pyramid Phase 2.5-3"
gh_create_issue \
  "Testing pyramid: Phase 2.5 (missing VM coverage) and Phase 3 (smoke tests)" \
  '## Context

Phases 1 and 2 are complete. Remaining phases:

**Phase 2.5 — Missing VM test coverage:**
- Agenix decryption: generate test keypair, create dummy secrets, verify at /run/agenix/
- WiFi bootstrap: test service copies .nmconnection when absent
- Docker: verify docker.service starts on isDev hosts
- SSH hardening: verify sshd config rejects password auth at runtime
- Firewall: verify specific ports blocked/allowed
- Impermanence: mock btrfs persist layout, verify dirs with correct ownership

**Phase 3 — Smoke tests:**
- `modules/tests/smoke.sh`: SSH into live host, verify real-world state
- Run post build-switch

## Deliverables

- [ ] Add agenix decryption VM test to `modules/tests/vm.nix`
- [ ] Add WiFi bootstrap VM test
- [ ] Add Docker startup VM test (isDev suite)
- [ ] Add SSH hardening runtime VM test
- [ ] Add firewall port VM test
- [ ] Add impermanence ownership VM test
- [ ] Write `modules/tests/smoke.sh` with SSH-based checks
- [ ] Document smoke test workflow in CLAUDE.md Testing section' \
  "scope:testing,infra,impact:medium,urgency:later,phase:S0" \
  "S0: Foundation"

# ---------------------------------------------------------------------------
# LOW PRIORITY
# ---------------------------------------------------------------------------

echo "[23/25] Nix lib for common module patterns"
gh_create_issue \
  "Create modules/_lib/ with helpers for common Nix module patterns" \
  '## Context

Repeated patterns across modules: `let hS = config.hostSpec; in`, Darwin guards (`lib.optionalAttrs (!hS.isDarwin)`), deferred module boilerplate, persistence wiring.

**When to do this:** When scope count exceeds ~15. Currently ~10 — hold off to avoid premature abstraction.

## Deliverables

- [ ] Create `modules/_lib/default.nix` with helpers: `mkScopedModule`, `withPersistence`, `ifNotDarwin`
- [ ] Refactor 2-3 existing scope modules to use the helpers as proof-of-concept
- [ ] Ensure `_lib/` is excluded from import-tree (already excluded by `_` prefix)
- [ ] Import `_lib` explicitly in `mk-host.nix` or flake-level lib
- [ ] Add eval test asserting helpers produce correct output' \
  "scope:core,refactor,impact:low,urgency:later,phase:S0" \
  "S0: Foundation"

echo "[24/25] Extract shared zsh values"
gh_create_issue \
  "Extract shared zsh values to avoid drift between wrapperrc.zsh and zsh.nix" \
  '## Context

`_config/zsh/wrapperrc.zsh` and `core/_home/zsh.nix` express the same settings in different formats. Drift has happened before.

Idea: create `_config/zsh/env.nix` with shared values consumed by both.

## Deliverables

- [ ] Audit current differences between `_config/zsh/wrapperrc.zsh` and `core/_home/zsh.nix`
- [ ] Decide on a shared values format (Nix file consumed by both, or generated shell file)
- [ ] Implement chosen approach and update both consumers
- [ ] Add a CI check (Phase 5 script) to detect drift going forward
- [ ] Update config-dependencies.md rule with the new shared source' \
  "scope:core,refactor,impact:low,urgency:later,phase:S0" \
  "S0: Foundation"

echo "[25/25] Explore hjem as HM alternative"
gh_create_issue \
  "Explore hjem as a minimal home-manager alternative" \
  "## Context

[hjem](https://github.com/feel-co/hjem) is a minimal file-symlinking alternative to home-manager. Vimjoyer uses it with full wrappers (zero HM).

Only worth exploring if HM becomes a pain point.

## Deliverables

- [ ] Read hjem docs and compare feature set with current HM usage
- [ ] Identify HM features we rely on: catppuccin/nix homeModules, programs.*, home.persistence
- [ ] Assess migration cost and benefits
- [ ] Decision: pursue migration or close as won't-do
- [ ] If pursuing: open child issues for each scope module migration" \
  "scope:core,refactor,impact:low,urgency:later,phase:S0" \
  "S0: Foundation"

echo ""
echo "==> All 25 issues created."
echo ""
echo "Verifying..."
gh issue list -R "$REPO" --limit 30 --state open
