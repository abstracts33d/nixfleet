# NixFleet Product

Open-core NixOS fleet management framework.

## Business Model

| Tier | Price | Target | Key features |
|------|-------|--------|-------------|
| **Community** | Free | <10 machines | Core CLI, Apache 2.0 |
| **Pro** | EUR 499-2,999/mo | 10-200 machines | Dashboard, audit logs, RBAC, binary cache |
| **Enterprise** | EUR 50k-500k/yr | 200+ machines | SSO/SAML, SLA, on-prem CP, NIS2 compliance |
| **Sovereign** | Custom, EUR 100k+ ACV | Government/defense | Air-gapped, source escrow, ANSSI/BSI |

Repo strategy: `nixfleet/` (Apache 2.0) + `nixfleet-platform/` (proprietary) + client repos.

## User Profiles

| Profile | Role | NixFleet config |
|---------|------|----------------|
| Developer | IDE, git, terminal | `workstation` (isDev, isGraphical) |
| Office worker | Email, browser, office suite | `office-workstation` (isGraphical, !isDev) |
| Field worker | Locked-down browser | `kiosk` (isGraphical, isMinimal) |
| Server | Headless infrastructure | `server` (isServer, !isGraphical) |

## Day-2 Ops Gaps

| Capability | Status |
|------------|--------|
| Push deployment to N machines | S3 agent MVP done |
| Rolling/canary deployment | Missing |
| Real-time deployment status | S4 CP MVP done |
| Fleet-wide rollback | Manual per-machine |
| Secrets rotation without downtime | Missing |
| CVE visibility | Missing (vulnix planned) |

## Production-Ready Criteria

1. Automated provisioning (< 30 min/host)
2. Config changes deployed in < 5 min across fleet
3. Rollback < 90s per host
4. Health check after every deployment
5. Audit trail for every config change
6. Secrets rotation without downtime
