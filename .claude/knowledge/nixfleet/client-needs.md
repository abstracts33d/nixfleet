# Client Needs Per Tier

Distilled knowledge about what real-world clients need from NixFleet.

## Day-2 Operations Gap

Day-2 ops (ongoing management) occupy 90% of a fleet's lifetime. Current gaps:

| Capability | Status | Effort |
|------------|--------|--------|
| Push deployment to N machines | Missing (need fleet agent S3) | S3 implementation |
| Rolling deployment (canary) | Missing | Deployment strategies |
| Deployment status (real-time) | Missing | Agent heartbeat + control plane S4 |
| Adding new hosts (CLI) | Manual process | 2-3 weeks CLI scaffolding |
| Fleet-wide rollback | Manual per-machine | 2-3 weeks orchestration |
| Secrets rotation | Manual, requires full rebuild | 3-4 weeks basic, months for Vault |
| OS updates with CVE visibility | Manual `nix flake update` | 2 weeks vulnix, 1 month staged pipeline |

## User Profiles Beyond Developers

Most organizations are not developer-centric. A 50-person French PME might have 5 developers, 30 office workers, 10 field workers, 5 managers.

| Profile | Key Applications | NixFleet Role |
|---------|-----------------|---------------|
| Developer | IDE, git, Docker, terminal | `workstation` (isDev, isGraphical) |
| Office worker | Email, office suite, browser, file sharing | `office-workstation` (isGraphical, !isDev, usePrinting, useFilesharing) |
| Manager | Office + video conferencing, dashboards | `executive` (extends office) |
| Administrative staff | Accounting software, CRM, HR tools | `admin-workstation` (extends office + enterprise-apps) |
| Field worker | Locked-down browser, single app | `kiosk` (isGraphical, isMinimal) |
| Presentation room | Browser, video conf, no persistent user | `presentation` (isGraphical, isMinimal) |

## Tier Feature Matrix

### Community (free, <10 machines)
- Core CLI: `mkFleet`, `mkOrg`, `mkRole`, `mkHost`
- Eval + VM testing
- Manual deployment via `build-switch` / `nixos-anywhere`

### Pro (EUR 499-2,999/mo, 10-200 machines)
- Dashboard UI with fleet overview
- Audit logs + deployment history
- RBAC (role-based access control)
- Binary cache hosting (Attic)
- CVE scanning against SBOM
- Staged update pipeline (dev -> staging -> prod)
- Deployment approval gates

### Enterprise (EUR 50k-500k/yr, 200+ machines)
- SSO/SAML integration
- On-premises control plane
- NIS2 compliance package (SBOM, incident timeline, audit reports)
- Auto-update with approval gates
- SLA guarantee
- Custom integrations

### Sovereign (EUR 100k+ ACV, government/defense)
- Air-gapped deployment
- Source code escrow
- ANSSI/BSI certification support
- On-site deployment and training

## Acceptance Criteria for Production-Ready

From client research, a fleet must demonstrate:
1. Automated provisioning (< 30 min for a new host)
2. Configuration changes deployed in < 5 min across fleet
3. Rollback capability (< 90s for any single host)
4. Health check verification after every deployment
5. Audit trail for every configuration change
6. Secrets rotation without downtime
7. CVE visibility for the full software stack
