---
name: onboard
description: Onboard a new organization onto NixFleet — needs analysis, architecture, fleet setup, documentation.
user-invocable: true
argument-hint: "<org-name> <description>"
---

# New Client Onboarding

## Input

The user provides `<org-name>` and a `<description>` of the organization's needs. If not provided, ask:
- "What is the organization name?"
- "Describe their environment: how many hosts, what roles, what platform (NixOS / macOS / mixed), any enterprise requirements?"

## Process

### Stage 1 — Needs Analysis (product-analyst)

Dispatch `product-analyst` with the org name and description:
- Map stated needs to NixFleet tiers (starter / team / enterprise)
- Identify required hostSpec flags per role (server, workstation, minimal, graphical, dev, etc.)
- Surface enterprise scope requirements: VPN, LDAP, printing, filesharing, corporate certs, proxy
- Recommend a role taxonomy for this org's fleet
- Output: needs summary + tier recommendation + required flags per role

### Stage 2 — Fleet Architecture (architect)

Dispatch `architect` with Stage 1 output:
- Design the fleet topology: host roles, scope activation matrix
- Propose `hostSpecDefaults` for the org's common baseline
- Identify any missing scopes or flags that need to be created first
- Assess impermanence strategy: which hosts need ephemeral root?
- Assess secrets layout: which secrets are org-wide vs host-specific?
- Output: architecture design with host role definitions

### Stage 3 — Org Config Scaffold (nix-expert)

Dispatch `nix-expert` with Stage 1+2 output:
- Generate the `mkOrg` entry (or equivalent fleet.nix block) for the new org:
  ```nix
  # fleet.nix or modules/orgs/<org-name>.nix
  {
    org = "<org-name>";
    tier = "<tier>";
    hostSpecDefaults = { ... };
    hosts = [
      { name = "<hostname>"; role = "<role>"; ... }
    ];
  }
  ```
- Wire any required enterprise scopes that aren't yet stubbed
- Verify the config evaluates without errors
- Output: scaffold Nix code + any issues found

### Stage 4 — Deployment Workflow (fleet-ops)

Dispatch `fleet-ops` with Stage 1–3 output:
- Define the deployment workflow for this org:
  - Install sequence (nixos-anywhere target order)
  - Secret provisioning steps (agenix keys, WiFi bootstrap)
  - Rollback plan per host role
- Generate a runbook: `docs/orgs/<org-name>/runbook.md`
- Identify monitoring and health check approach
- Output: deployment workflow + runbook path

### Stage 5 — Documentation (doc-writer)

Dispatch `doc-writer` with all previous output:
- Create `docs/orgs/<org-name>/README.md` with:
  - Org overview, tier, contact
  - Host inventory table
  - Scope activation matrix
  - Secret inventory (names only, no values)
  - Deployment runbook link
- Update `docs/src/nixfleet/clients.md` with the new org entry
- Update README.md fleet table if applicable
- Output: list of created/updated doc files

### Stage 6 — Present

```
## Onboarding Complete — <org-name>

### Fleet Summary
- Tier: <tier>
- Hosts: N (<roles>)
- Platform: NixOS / macOS / mixed
- Enterprise scopes: [list]

### Next Steps
1. Provision decryption keys (see runbook)
2. Run install sequence: nix run .#install -- --target root@<ip> -h <hostname> -u <username>
3. Verify with: nix run .#validate

### Files Created
- modules/orgs/<org-name>.nix (fleet config)
- docs/orgs/<org-name>/README.md (runbook + docs)
- docs/src/nixfleet/clients.md (updated)

### fleet.nix entry:
<generated nix code>
```

## Verification

Before presenting, invoke `superpowers:verification-before-completion`:
- Show that the Nix scaffold evaluates (nix-expert must run eval)
- Show that doc files exist on disk
- Never claim "ready to deploy" without a working config
