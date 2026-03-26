# NixFleet: Real-World Client Needs Per Tier

**Date:** 2026-03-25
**Status:** Research Report
**Author:** Product Strategy Research

---

## Table of Contents

1. [Day-2 Operations Gap Analysis](#1-day-2-operations-gap-analysis)
2. [User Profiles Beyond Developers](#2-user-profiles-beyond-developers)
3. [Desktop Application Stacks](#3-desktop-application-stacks)
4. [Community Tier](#4-community-tier)
5. [Pro Tier](#5-pro-tier)
6. [Enterprise Tier](#6-enterprise-tier)
7. [Sovereign Tier](#7-sovereign-tier)
8. [Windows-to-NixOS Migration Guide](#8-windows-to-nixos-migration-guide)
9. [Acceptance Criteria for Production-Ready](#9-acceptance-criteria-for-production-ready)
10. [Sources](#sources)

---

## 1. Day-2 Operations Gap Analysis

The current NixFleet framework (`mkFleet`, `mkOrg`, `mkRole`, `mkHost`) handles Day-0 (initial provisioning) and Day-1 (initial configuration) well. Day-2 operations -- the ongoing management that occupies 90% of a fleet's lifetime -- are the critical gap.

### 1.1 Fleet Config Updates and Deployment

**Current state:** No fleet deployment mechanism. A user must SSH into each machine or use `nixos-rebuild --target-host` one at a time.

**What clients need:**

| Capability | Description | Existing Nix Tooling | NixFleet Gap |
|------------|-------------|---------------------|--------------|
| Push deployment | Apply config changes to N machines in parallel | Colmena, deploy-rs, NixOps | No orchestration layer; need fleet agent (S3) |
| Rolling deployment | Deploy to canary group first, then progressively | None in Nix ecosystem | Entirely missing; need deployment strategies |
| Deployment status | Real-time view of which machines are updated | None | Need agent heartbeat + control plane (S4) |
| Pre-flight validation | Dry-run build on target before switch | `nixos-rebuild dry-activate` | Need fleet-wide dry-run orchestration |
| Deployment approval | Require human approval before fleet-wide switch | None | Need RBAC + approval workflow (Pro+) |

**Minimum viable deployment flow:**
```
1. User edits fleet config (git commit)
2. CI builds all affected closures (binary cache)
3. Fleet agent on each machine pulls its closure
4. Agent runs `switch-to-configuration` with rollback timer
5. Control plane shows deployment progress
6. If health checks fail within 5 min, auto-rollback
```

**Colmena comparison:** Colmena provides parallel push deployment with tag-based host selection (`--on @web`), but is stateless (no deployment history, no approval workflows, no health checks). NixFleet must match Colmena's deployment speed while adding enterprise features on top.

### 1.2 Adding a New Host

**Current state:** Manually create a host file in `modules/hosts/`, add hardware config, run `nixos-anywhere`.

**What clients need:**
- CLI command: `nixfleet host add --org acme --role workstation --hostname desk-042`
- Auto-generates host file from role template
- Auto-provisions secrets (agenix key generation + encryption)
- Auto-registers in fleet inventory
- For batch provisioning: `nixfleet host add-batch --org acme --role edge --count 50 --prefix edge-`

**Effort:** 2-3 weeks for CLI scaffolding + template system.

### 1.3 Adding a New Role/Profile

**Current state:** Manually define a role with `mkRole` and compose hostSpec flags.

**What clients need:**
- CLI: `nixfleet role create --name kiosk --base minimal --flags "isGraphical useProxy"`
- Role wizard that shows available flags and their implications
- Role testing: `nixfleet role test kiosk` builds a VM with that role and runs eval + VM tests
- Role diffing: `nixfleet role diff workstation kiosk` shows configuration differences

**Effort:** 1-2 weeks for CLI, 1 week for test integration.

### 1.4 Fleet-Wide Rollback

**Current state:** Per-machine `nixos-rebuild switch --rollback` or boot into previous generation.

**What clients need:**

| Scenario | Required Capability |
|----------|-------------------|
| Bad deploy to 200 machines | Fleet-wide rollback to previous known-good generation in <5 min |
| Partial failure (50/200 failed) | Selective rollback of failed machines only |
| Config regression discovered days later | Rollback to specific generation by number or timestamp |
| Rollback with data migration | Rollback config but preserve data changes (e.g., database migrations) |

**Implementation:** The fleet agent must track generation history per machine. The control plane must provide a "rollback fleet to generation N" command that instructs all agents to switch. NixOS already supports this at the single-machine level -- the gap is orchestration.

**Effort:** 2-3 weeks (depends on fleet agent maturity).

### 1.5 Secrets Rotation

**Current state:** Manual process: edit secrets in nix-secrets repo, re-encrypt with agenix, commit, push, `nix flake update secrets`, rebuild.

**What clients need:**
- Automated rotation schedules (e.g., rotate SSH host keys every 90 days)
- Rotation without full rebuild (hot-reload secrets where possible)
- Audit trail: who rotated what, when, which machines received the new secret
- Emergency rotation: rotate a compromised key across the entire fleet immediately
- Integration with HashiCorp Vault or similar for Pro+ tiers

**Gap analysis:** agenix is file-based and requires a full rebuild to rotate secrets. For the Community tier, this is acceptable. For Pro+, NixFleet needs a secrets management layer that can push secret updates independently of config changes. The roadmap lists "agenix / sops-nix -> Vault (Pro tier)" but no concrete design exists.

**Effort:** 3-4 weeks for basic rotation CLI, 2-3 months for Vault integration.

### 1.6 OS Updates (nixpkgs Bumps, Security Patches)

**Current state:** Manual `nix flake update` bumps nixpkgs. No visibility into what changed or what CVEs are patched.

**What clients need:**

| Capability | Community | Pro | Enterprise |
|------------|-----------|-----|------------|
| `nix flake update` with changelog | X | X | X |
| CVE scan of current closure vs. updated closure | | X | X |
| Staged update (dev -> staging -> prod) | | X | X |
| Auto-update with approval gates | | | X |
| SBOM diff between generations | | | X |
| Compliance report (which CVEs are now patched) | | | X |

**Key insight:** Enterprises do not want "latest nixpkgs." They want a validated baseline that they control. NixFleet should offer curated nixpkgs channels (similar to how RHEL provides stable streams) or at minimum a "pin + validate + promote" workflow.

**Effort:** CVE scanning (vulnix integration): 2 weeks. Staged update pipeline: 1 month. Curated channels: ongoing operational cost.

---

## 2. User Profiles Beyond Developers

The original analysis focused on developer-centric workloads. In reality, developers are a minority in most organizations. A 50-person French PME might have 5 developers, 30 office workers, 10 field/on-site workers, and 5 managers. NixFleet must serve ALL of them.

### 2.1 Profile Definitions

| Profile | Typical Users | Key Applications | NixFleet Role |
|---------|--------------|-----------------|---------------|
| **Developer** | Software engineers, DevOps, SRE | IDE, git, Docker, terminal, language toolchains | `role "dev-workstation" -> {isGraphical, isDev}` |
| **Office worker** | Assistants, HR, accounting, sales | Email, calendar, office suite, file sharing, web browser | `role "office-workstation" -> {isGraphical, !isDev, scope:office}` |
| **Manager / Executive** | Directors, C-suite, project managers | Same as office + video conferencing, presentation tools, dashboards | `role "executive" -> extends office-workstation + scope:communications` |
| **Administrative staff** | Accounting, HR, procurement | Accounting software (Sage, EBP), CRM, HR tools, often Windows-dependent | `role "admin-workstation" -> extends office-workstation + scope:enterprise-apps` |
| **Field worker** | Warehouse, logistics, retail, reception | Locked-down browser, single application, no admin access | `role "kiosk" -> {isGraphical, isMinimal, scope:kiosk}` |
| **Presentation room** | Conference rooms, training rooms | Browser, video conferencing, screen sharing, no persistent user | `role "presentation" -> {isGraphical, isMinimal, scope:communications}` |

### 2.2 Role-to-Scope Mapping

```nix
# Office workstation: the bread and butter of any organization
role "office-workstation" {
  hostSpec = {
    isGraphical = true;
    isDev = false;
    isMinimal = false;
    usePrinting = true;
    useFilesharing = true;
  };
  scopes = [ "office" "communications" ];
  # Result: email client, office suite, file manager, browser with policies,
  #         network printers, shared drives, video conferencing
}

# Kiosk: locked-down single-purpose machine
role "kiosk" {
  hostSpec = {
    isGraphical = true;
    isDev = false;
    isMinimal = true;  # No unnecessary packages
  };
  scopes = [ "kiosk" ];
  # Result: locked browser or single app, no admin access, no USB,
  #         auto-login, screen lock after timeout, no shell access
}

# Executive: office + premium communication tools
role "executive" {
  hostSpec = {
    isGraphical = true;
    isDev = false;
    usePrinting = true;
    useFilesharing = true;
  };
  scopes = [ "office" "communications" "executive" ];
  # Result: everything office + video conf (Jitsi/Teams/Zoom),
  #         presentation tools, calendar sync, mobile device pairing
}

# Admin workstation: office + enterprise applications
role "admin-workstation" {
  hostSpec = {
    isGraphical = true;
    isDev = false;
    usePrinting = true;
    useFilesharing = true;
  };
  scopes = [ "office" "enterprise-apps" ];
  # Result: everything office + accounting (web or Wine-based),
  #         CRM client, HR tools, RDP client for Windows-only apps
}
```

### 2.3 New hostSpec Flags Needed

| Flag | Default | Controls |
|------|---------|----------|
| `useOffice` | `false` | Office suite, email client, calendar, file manager |
| `useKiosk` | `false` | Kiosk mode (locked browser, auto-login, restricted shell) |
| `useCommunications` | `false` | Video conferencing, messaging clients |

These would follow the same smart-default pattern: `useKiosk` implies `isGraphical = true`, `isMinimal = true`, `isDev = false`.

### 2.4 Implications for Each Tier

- **Community:** Even small startups have 1-2 non-dev users (office manager, founder doing sales). They need a "just works" desktop without dev clutter.
- **Pro:** Non-technical users are the MAJORITY. The IT team's primary job is managing office workstations, not dev machines. The dashboard, help desk integration, and printing setup are more important than Docker configuration.
- **Enterprise:** Role-based provisioning is critical. A single fleet may contain dev machines, office machines, kiosks, presentation rooms, and server rooms. The role system must handle application stack differences, not just flag differences.
- **Sovereign:** Government offices are almost entirely non-technical users. The sovereign desktop (like GendBuntu) must be a complete replacement for Windows, including office productivity, document management, and citizen-facing kiosks.

---

## 3. Desktop Application Stacks

### 3.1 Application-to-Package Mapping

| Need | NixOS Package(s) | NixFleet Scope | Notes |
|------|------------------|----------------|-------|
| **Email** | `thunderbird`, `evolution` | `scope:office` | Thunderbird supports Exchange via TbSync plugin; Evolution has native EWS |
| **Calendar** | `thunderbird` (Lightning), `gnome-calendar`, `evolution` | `scope:office` | CalDAV for sovereign, Exchange for hybrid |
| **Office suite** | `libreoffice`, `onlyoffice-desktopeditors` | `scope:office` | LibreOffice is default; OnlyOffice for better M365 compat |
| **File sharing client** | `nextcloud-client`, `owncloud-client`, `kdrive` | `scope:office` | Depends on org backend |
| **Video conferencing** | `jitsi-meet` (web), `zoom-us`, `teams-for-linux` | `scope:communications` | Jitsi for sovereign; Zoom/Teams for hybrid |
| **Messaging** | `element-desktop`, `signal-desktop`, `slack` | `scope:communications` | Element for sovereign (Matrix); Slack/Teams for commercial |
| **Web browser** | `firefox`, `chromium` | `scope:graphical` (existing) | With managed policies (bookmarks, extensions, proxy) |
| **PDF viewer** | `evince`, `okular`, `firefox` | `scope:graphical` (existing) | Already covered by most desktops |
| **File manager** | `nautilus`, `thunar`, `dolphin` | `scope:graphical` (existing) | Must support SMB/CIFS network drives |
| **Image viewer** | `eog`, `loupe`, `feh` | `scope:graphical` (existing) | Basic need for any desktop |
| **Printing** | CUPS + `system-config-printer` | `scope:enterprise` (printing.nix) | Auto-discovery via Avahi, PPD management |
| **Scanning** | `simple-scan`, `xsane` | `scope:office` | SANE backend + org-managed scanner list |
| **Accounting** | Web-based (Dolibarr, ERPNext) or Wine + Sage/EBP | `scope:enterprise-apps` | Most French accounting is still Windows-based |
| **CRM** | Web-based (SuiteCRM, Odoo) | `scope:enterprise-apps` | Browser-only, no special packaging |
| **HR tools** | Web-based (Kiwihr, PayFit) | `scope:enterprise-apps` | Browser-only |
| **Remote desktop** | `remmina`, `freerdp` | `scope:enterprise-apps` | For accessing Windows-only apps via RDP |
| **Screen lock** | `swaylock`, `swayidle` (Wayland), `xscreensaver` (X11) | `scope:security` | Auto-lock after timeout, org-defined policy |
| **Disk encryption** | LUKS (dm-crypt) + TPM | core | Already standard NixOS, add key escrow |
| **USB control** | USBGuard | `scope:security` | Per-role USB policies, violation alerting |

### 3.2 Scope Module Design

New scopes needed beyond existing ones:

```
modules/scopes/
├── office/
│   ├── nixos.nix          # System-level office config (fonts, MIME defaults)
│   ├── home.nix           # HM: thunderbird, libreoffice, file sharing client
│   └── scanning.nix       # SANE + scanner auto-discovery
├── communications/
│   ├── nixos.nix          # PipeWire (already in graphical), camera access
│   └── home.nix           # HM: video conf client, messaging
├── kiosk/
│   ├── nixos.nix          # Auto-login, restricted shell, USB lockdown
│   └── home.nix           # HM: managed browser in kiosk mode
├── security/
│   ├── screen-lock.nix    # Idle timeout + lock policy
│   └── usb-control.nix    # USBGuard per-role policies
└── enterprise/            # (existing, extend)
    ├── remote-desktop.nix # Remmina + FreeRDP for Windows app access
    └── enterprise-apps.nix # Accounting/CRM browser bookmarks + shortcuts
```

### 3.3 Browser Policy Management

Managed browsers are essential for non-technical users. Firefox and Chromium both support enterprise policies:

```nix
# Example: org-level Firefox policy pushed to all office workstations
programs.firefox.policies = {
  Homepage.URL = "https://intranet.company.fr";
  Bookmarks = [
    { Title = "Webmail"; URL = "https://mail.company.fr"; }
    { Title = "Nextcloud"; URL = "https://cloud.company.fr"; }
    { Title = "ERP"; URL = "https://erp.company.fr"; }
  ];
  ExtensionSettings = {
    "uBlock0@raymondhill.net".installation_mode = "force_installed";
  };
  DisablePrivateBrowsing = true;  # For kiosk/compliance
  Proxy = { Mode = "system"; };
};
```

This maps directly to NixFleet org-level configuration: the org defines browser policies, and they propagate to all machines with the `office` or `kiosk` scope.

---

## 4. Community Tier

### Persona: Small Dev Team (+ Office Staff)

**Profile:** 5-10 developers at a startup or open-source project. 3-8 NixOS workstations, 1-2 CI servers. One person (the "Nix person") manages the fleet part-time. Budget: zero for tooling; time is the constraint.

**Often overlooked:** Even a 5-person startup has non-dev users. The founder doing sales, the office manager handling invoices, the part-time accountant. They need a desktop that works out of the box without understanding Nix.

**Current tools they are migrating FROM:**
- Ansible playbooks maintained by one person
- Docker Compose on dev machines
- Manual SSH + bash scripts
- Individual dotfiles repos
- Some may already use NixOS individually but not as a managed fleet

### Must-Have Features (Blocking Adoption)

- **Feature:** Single-command fleet deployment
  - Description: `nixfleet deploy` pushes config to all machines in parallel, equivalent to Colmena but integrated with the framework
  - Scope: Core (Apache 2.0)
  - Effort: 3-4 weeks (wrapping Colmena or building on nix-copy-closure + SSH)

- **Feature:** Getting-started wizard
  - Description: `nixfleet init` creates a fleet repo from an existing NixOS config, auto-detecting hosts and generating `mkFleet` structure
  - Scope: Core
  - Effort: 2 weeks

- **Feature:** Ansible migration guide + tooling
  - Description: Documentation mapping common Ansible patterns to NixOS modules. Tool that reads an Ansible inventory and generates `mkHost` stubs
  - Scope: Core (docs), Community (tool)
  - Effort: 1 week (docs), 2 weeks (tool)

- **Feature:** Binary cache setup
  - Description: One-command local binary cache (Attic) so builds are not repeated across machines. `nixfleet cache init` sets up a cache on one machine, configures all others to use it
  - Scope: Core (self-hosted), Pro (managed)
  - Effort: 1-2 weeks

- **Feature:** Basic monitoring
  - Description: `nixfleet status` shows generation number, last deploy time, nixpkgs version, and reboot-pending status for each host
  - Scope: Core (CLI)
  - Effort: 1-2 weeks

### Nice-to-Have Features

- **Feature:** Fleet-wide `nix flake update` with diff
  - Description: Shows what packages changed, what CVEs are addressed, before applying
  - Effort: 2 weeks

- **Feature:** Template library
  - Description: Pre-built role templates (dev workstation, CI runner, NAS, home server) that users can import
  - Effort: Ongoing

- **Feature:** Community forum / Discord
  - Description: Support channel for troubleshooting
  - Effort: Operational

### Software Suites Needed

**For developers (existing):**

| Category | Tools |
|----------|-------|
| Development | git, neovim/vscode, docker, direnv, mise, language toolchains |
| CI/CD | Gitea/Forgejo self-hosted, or GitHub Actions runners |
| Monitoring | Prometheus + Grafana (basic), or just systemd journal |
| Communication | Matrix/Element or Slack (not managed by NixFleet) |
| File sharing | Syncthing or NFS between machines |

**For office workers (new):**

| Category | Tools |
|----------|-------|
| Email | Thunderbird with autoconfig (company IMAP/SMTP or web-based) |
| Office suite | LibreOffice (sufficient for a startup) |
| File sharing | Nextcloud or Syncthing |
| Web browser | Firefox with org bookmarks (intranet, invoicing tool, CRM) |
| Printing | CUPS (if they have a printer; most startups do) |

**Key insight:** At the Community tier, the "office-workstation" role is just the "dev-workstation" minus dev tools plus LibreOffice and Thunderbird. A single template role covers it. The "Nix person" should not have to configure office software manually -- `nixfleet role use office-workstation` should just work.

### Migration Path

**From Ansible:**
1. Map Ansible inventory groups to NixFleet organizations + roles
2. For each Ansible role, identify the equivalent NixOS module or create one
3. Key selling point: Ansible roles drift over time; NixOS modules are guaranteed reproducible
4. Pain point: Ansible users expect imperative "do X then Y" thinking; NixOS is declarative
5. Need: excellent error messages when a user tries to do something imperatively

**From Docker Compose:**
1. Many dev teams use Docker for local dev environments. NixFleet replaces system config, not application containers
2. Docker still runs on NixFleet-managed machines (the `isDev` scope enables Docker)
3. Migration: keep Docker for apps, use NixFleet for the OS layer

**From manual config:**
1. The hardest migration -- no existing automation to map from
2. Need: audit tool that SSHes into existing machines and generates NixOS module approximations
3. `nixfleet import --host root@192.168.1.10` could inspect the machine and generate a starter config

### Minimum Viable Onboarding Experience

```
# 1. Install NixFleet CLI (works on any Nix machine)
nix run github:nixfleet/nixfleet#init -- --org myteam

# 2. Add existing NixOS machines
nixfleet host add --hostname dev-01 --role workstation --ssh root@dev-01

# 3. Deploy
nixfleet deploy

# 4. Check status
nixfleet status
```

Time from zero to managed fleet: **under 30 minutes** or they will abandon it.

---

## 5. Pro Tier

### Persona: French PME/SMB

**Profile:** A 50-200 person company in France. 50-200 endpoints (mix of workstations and servers). Has an IT team of 2-5 people. Regulatory pressure from GDPR and incoming NIS2. Currently running Windows + Active Directory + Microsoft 365, considering alternatives for cost and sovereignty reasons. Budget: EUR 500-3,000/month for endpoint management.

### Workforce Breakdown (Typical 50-Person PME)

| Role | Count | Profile | NixFleet Role |
|------|-------|---------|---------------|
| Developers / IT | 5 | IDE, Docker, terminal | `dev-workstation` |
| Office workers | 25 | Email, office suite, file sharing | `office-workstation` |
| Sales / field | 8 | CRM, email, browser | `office-workstation` |
| Managers | 5 | Video conf, dashboards, presentations | `executive` |
| Accounting / HR | 5 | Sage/EBP, payroll, HR tools | `admin-workstation` |
| Reception / shared | 2 | Locked browser, phone system | `kiosk` |

**Critical realization:** 80% of the fleet is non-developer machines. The Pro tier's success depends entirely on serving office workers well, not developers. The IT team managing the fleet cares about printer setup, email auto-configuration, and file share mounting -- not Docker volumes.

### Non-Technical User Requirements (Pro)

These are blocking requirements for any PME adoption:

| Requirement | Description | NixFleet Scope |
|-------------|-------------|----------------|
| **Sovereign email** | KSuite (Infomaniak), ProtonMail Business, or self-hosted (Mailu, iRedMail). Pre-configured in Thunderbird at first boot | `scope:office` + org config |
| **Office suite** | LibreOffice or OnlyOffice with company templates pre-installed. Must open .docx/.xlsx without issues | `scope:office` |
| **File sharing** | Nextcloud client or kDrive syncing to ~/Documents. Network drives (SMB) auto-mounted | `scope:office` + `useFliesharing` |
| **Video conferencing** | Jitsi (sovereign) or Teams/Zoom (hybrid). Camera and microphone must work out of the box | `scope:communications` |
| **Printing** | CUPS with org-defined printers. Users should see printers immediately, no manual setup | `usePrinting` (existing) |
| **Directory / user mgmt** | FreeIPA or AD integration. Single password for desktop + email + file share | `useLdap` (existing) |
| **Browser policies** | Corporate bookmarks, homepage, proxy settings, forced extensions (uBlock) | `scope:office` |
| **Endpoint security** | Auto screen lock (5 min), disk encryption (LUKS + escrow), USB control (no unauthorized sticks) | `scope:security` |
| **French keyboard / locale** | AZERTY layout, French spell-check in LibreOffice and Thunderbird, EUR symbol | core + `scope:office` |

**Typical French PME IT Stack (as-is):**
- Windows 10/11 on desktops, managed via GPO or SCCM
- Active Directory for authentication
- Microsoft 365 (Exchange, Teams, SharePoint, OneDrive)
- Sage 100 or SAP Business One for ERP (often on-premise, sometimes AS/400 legacy)
- OVHcloud or Scaleway for hosting
- GLPI for IT asset management and helpdesk
- Basic antivirus (Kaspersky, ESET, or Windows Defender)
- VPN via Fortinet or Palo Alto appliance
- Network printers (HP, Ricoh) via print server

### Must-Have Features (Blocking Adoption)

- **Feature:** Dashboard with fleet inventory
  - Description: Web UI showing all machines, their roles, generation history, last check-in, OS version. Searchable, filterable. This is what IT managers expect from any endpoint management tool
  - Scope: Pro
  - Effort: 2-3 months (extend current Go dashboard)

- **Feature:** RBAC with audit logging
  - Description: Multiple IT staff with different permission levels (admin, deployer, viewer). Every action logged with who/what/when. Required for GDPR accountability
  - Scope: Pro
  - Effort: 1-2 months

- **Feature:** Active Directory / LDAP integration
  - Description: Users authenticate against existing AD. NixOS machines join the domain. sudo rules derived from AD groups. This is THE blocker for any enterprise moving from Windows
  - Scope: Pro (LDAP), Enterprise (full AD + Kerberos)
  - Effort: 2-3 weeks (LDAP via sssd), 2 months (full AD + Kerberos)

- **Feature:** Managed binary cache
  - Description: EU-hosted binary cache (Attic on OVHcloud/Scaleway). The PME should not have to run their own cache infrastructure. Per-org namespace isolation
  - Scope: Pro (SaaS)
  - Effort: 1-2 months for hosting infrastructure

- **Feature:** GLPI integration
  - Description: Auto-populate GLPI CMDB with hardware/software inventory from fleet. Sync asset lifecycle. GLPI is the de facto ITSM tool for French SMBs (open-source, widely deployed). The integration should push CIs (Configuration Items) to GLPI via its REST API
  - Scope: Pro
  - Effort: 2-3 weeks

- **Feature:** Network printing (CUPS + org-managed printers)
  - Description: Declarative printer list from org config, auto-discovered via Avahi, with PPD drivers managed centrally. IT staff configure printers once in the fleet config; all workstations get them
  - Scope: Pro
  - Effort: 1-2 weeks (NixOS already has good CUPS support; the gap is org-level declaration)

- **Feature:** File sharing (SMB/CIFS mounts)
  - Description: Mount corporate file shares with credentials from agenix. Auto-mount via autofs. Integration with file manager (Nautilus/Thunar). Essential for any office that has shared drives
  - Scope: Pro
  - Effort: 1-2 weeks

- **Feature:** Email client pre-configuration
  - Description: Thunderbird with IMAP/Exchange auto-configured from org settings. If the PME uses Microsoft 365 (most do), the Exchange/OAuth2 integration must work out of the box
  - Scope: Pro
  - Effort: 1-2 weeks (Thunderbird + autoconfig)

- **Feature:** Endpoint backup
  - Description: Automated backup of user data via Restic/Borg to org-managed backup server. Retention policies defined at the org level. Essential for GDPR (data protection) and business continuity
  - Scope: Pro
  - Effort: 2-3 weeks

### Nice-to-Have Features

- **Feature:** Microsoft 365 web app integration
  - Description: Browser bookmarks/PWAs for Teams, Outlook Web, SharePoint pre-configured. Managed Firefox/Chrome policies
  - Effort: 1 week

- **Feature:** VPN with split tunneling
  - Description: Auto-connect to corporate VPN, route only corporate traffic through it
  - Effort: 1-2 weeks

- **Feature:** Disk encryption with escrow
  - Description: LUKS + TPM with recovery key escrowed to org vault
  - Effort: 2 weeks

- **Feature:** Centralized logging (journald to Loki/Elastic)
  - Description: Forward system logs to a central aggregator for troubleshooting
  - Effort: 1-2 weeks

### KSuite (Infomaniak) Integration

KSuite is a Swiss-hosted sovereign office suite offering:
- **kDrive:** Cloud storage (alternative to OneDrive/Google Drive)
- **kMeet:** Video conferencing (alternative to Teams/Meet)
- **kChat:** Messaging (alternative to Slack/Teams chat)
- **OnlyOffice integration:** Document editing (alternative to Google Docs/Office Online)
- **Infomaniak Mail:** Email with calendar and contacts
- **Sovereign AI:** RAG-based document search, translation, summarization

**NixFleet integration points:**
1. **kDrive client on NixOS:** Package the kDrive desktop sync client (currently available for Linux). Deploy via scope module
2. **Mail auto-configuration:** Pre-configure Thunderbird with Infomaniak IMAP settings from org config
3. **kMeet:** Browser-based (no special integration needed beyond managed bookmarks)
4. **SSO:** Infomaniak supports SAML/OpenID Connect -- integrate with NixFleet's SSO module for Enterprise tier

**Strategic value:** KSuite positions itself as the European GDPR-compliant alternative to Google Workspace. A NixFleet + KSuite bundle would be a compelling "fully sovereign desktop" offering for French PMEs looking to de-GAFAM.

### Migration Path

**From Windows + AD + Microsoft 365** (see [Section 8](#8-windows-to-nixos-migration-guide) for detailed guide):
1. Phase 1 (pilot): Deploy NixOS on 5-10 non-critical workstations (reception, shared spaces). Keep AD, mount same file shares, use M365 via browser
2. Phase 2 (IT team): Migrate IT team workstations. They become the internal champions
3. Phase 3 (department): One department at a time. LibreOffice for document editing, Thunderbird for email, Firefox for M365 web apps
4. Phase 4 (full): AD -> FreeIPA/Keycloak migration for remaining Windows machines

**Key risks for non-technical users:**
- Users resistant to change (budget EUR 200-500/person for training)
- LibreOffice formatting issues with complex .docx templates (invoices, contracts) -- mitigate with OnlyOffice
- Sage 100 ERP (Windows-only; needs remote desktop to Windows server)
- Peripheral drivers (scanners, specialized hardware -- test BEFORE migration)
- Printing regressions (test every printer model with CUPS + correct PPD)
- Calendar/contact sync issues when moving from Exchange to CalDAV/CardDAV

**Hybrid coexistence (critical for PME):**
- NixOS and Windows machines MUST coexist on the same network for 6-12 months
- AD trust with FreeIPA allows single sign-on across both platforms
- Samba file shares accessible from both Windows and NixOS
- RDP client (Remmina) pre-installed for accessing Windows-only applications
- Managed Firefox on NixOS with M365 web apps (Outlook Web, Teams Web) as fallback

**From Ansible:**
- Similar to Community tier but with emphasis on migrating GPO policies to NixOS modules
- Map AD GPO security settings to NixOS hardening modules

---

## 6. Enterprise Tier

### Persona: Large French/European Enterprise

**Profile:** A CAC 40 or SBF 120 company, or a large public institution. 500-5,000+ endpoints. Dedicated IT security team (CISO + 5-20 security engineers). Multiple sites across France/Europe. Subject to NIS2 (essential or important entity). Heavy compliance requirements. Budget: EUR 50k-500k/year. Current tooling: SCCM/Intune for Windows, ServiceNow for ITSM, CrowdStrike/SentinelOne for EDR.

### Fleet Composition (Typical 1,000-Endpoint Enterprise)

| Role | Count | Profile | NixFleet Role |
|------|-------|---------|---------------|
| Developers / IT | 80 | IDE, Docker, CI tools | `dev-workstation` |
| Office workers | 500 | Email, office suite, ERP web client | `office-workstation` |
| Sales / field | 100 | CRM, mobile sync, VPN | `office-workstation` + VPN |
| Managers / executives | 50 | Video conf, BI dashboards, presentations | `executive` |
| Accounting / finance | 40 | SAP/Sage, banking apps, auditing tools | `admin-workstation` |
| HR / legal | 30 | HR suite, document management, e-signatures | `admin-workstation` |
| Reception / kiosks | 20 | Locked browser, visitor management, digital signage | `kiosk` |
| Conference rooms | 30 | Video conf endpoint, screen sharing, no user state | `presentation` |
| Servers / infra | 150 | Headless, services, databases | `server` |

**Enterprise-specific challenge:** Unlike the Pro tier where a handful of roles suffice, enterprises need DEPARTMENT-LEVEL role customization. The finance department gets SAP access; legal gets document management; marketing gets design tools. NixFleet's role system must support role INHERITANCE and COMPOSITION:

```
role "base-office" -> common office stack
role "finance"     -> extends base-office + SAP web, banking apps, dual monitor
role "legal"       -> extends base-office + document management, e-signature, scanner
role "marketing"   -> extends base-office + design tools (GIMP, Inkscape), large display
role "executive"   -> extends base-office + video conf, BI dashboards
```

### Application Stack Differences by Department

| Department | Additional Apps (beyond base office) | Windows Dependency |
|------------|-------------------------------------|-------------------|
| Finance | SAP GUI (Wine/RDP), banking portals, dual-screen spreadsheets | HIGH (SAP GUI) |
| Legal | Document management (Alfresco), e-signature (DocuSign web), scanner | LOW |
| Marketing | GIMP, Inkscape, Figma (web), social media tools | LOW |
| HR | HR suite (web), payroll (web or RDP to Windows), training LMS | MEDIUM |
| Sales | CRM (web), video demos, screen recording | LOW |
| IT / Dev | Full dev stack (already covered) | NONE |
| Reception | Visitor management (web), phone system, badge printer | LOW |

### Must-Have Features (Blocking Adoption)

- **Feature:** SSO/SAML with Keycloak or Azure AD
  - Description: All NixFleet dashboard access via corporate SSO. Machine authentication via Kerberos tickets. Must support SAML 2.0, OpenID Connect, and Kerberos. Keycloak is the preferred open-source option; Azure AD for hybrid environments
  - Scope: Enterprise
  - Effort: 2-3 months

- **Feature:** NIS2 compliance module
  - Description: Must implement Article 21's 10 mandatory areas. Specific deliverables:
    - **Risk analysis:** Automated security posture assessment per host (CIS benchmarks, STIG modules)
    - **Incident handling:** Incident detection (auditd + AIDE), 24h early warning capability, 72h detailed report template, 1-month final report template
    - **Business continuity:** Backup verification, recovery time tracking, disaster recovery runbook generation
    - **Supply chain security:** SBOM generation per host (nix closure -> CycloneDX/SPDX), dependency audit, nixpkgs provenance verification
    - **Secure development:** Config-as-code with Git audit trail (already inherent to NixOS)
    - **Vulnerability testing:** CVE scanning (vulnix), penetration test support (attestation)
    - **Cybersecurity training:** Training completion tracking (integration with LMS)
    - **Cryptography:** Encryption-at-rest attestation (LUKS), encryption-in-transit verification (TLS config audit)
    - **Access control:** MFA enforcement, privilege escalation logging, session management
    - **Secure communications:** MFA/encrypted channels for admin access
  - Scope: Enterprise
  - Effort: 3-6 months for core, ongoing for compliance framework updates

- **Feature:** ServiceNow CMDB integration
  - Description: Auto-sync fleet inventory to ServiceNow CMDB as Configuration Items. Map NixOS generations to ServiceNow change records. Link deployments to change tickets. ServiceNow is the dominant ITSM platform in French enterprises (Airbus, TotalEnergies, BNP Paribas all use it)
  - Scope: Enterprise
  - Effort: 1-2 months

- **Feature:** EDR integration
  - Description: Deploy and manage EDR agents (CrowdStrike Falcon, SentinelOne, or open-source alternatives like OpenEDR/Wazuh) across the fleet. CrowdStrike has a Linux agent; it must be packaged for NixOS and deployed via scope module. For sovereign deployments, Wazuh (open-source SIEM + EDR) is the alternative
  - Scope: Enterprise
  - Effort: 2-3 weeks per EDR vendor (packaging + NixOS module)

- **Feature:** On-premises control plane
  - Description: The entire NixFleet control plane (dashboard, RBAC, audit, cache) running on-premises in the customer's datacenter. No SaaS dependency. Must run on NixOS or as OCI containers on any Linux
  - Scope: Enterprise
  - Effort: 2-3 months (containerization + deployment automation)

- **Feature:** SLA with guaranteed response times
  - Description: 4h response for P1 (fleet-wide outage), 8h for P2 (single-host critical), 24h for P3. Dedicated support engineer who knows the customer's config
  - Scope: Enterprise (contractual, not technical)
  - Effort: Operational (requires staffing)

- **Feature:** Staged deployment pipelines
  - Description: Dev -> Staging -> Production deployment stages with automatic promotion gates. Canary deployments (5% -> 25% -> 100%). Automatic rollback on health check failure
  - Scope: Enterprise
  - Effort: 2-3 months

- **Feature:** FreeIPA or Keycloak directory service
  - Description: Full directory service integration replacing Active Directory. FreeIPA for Linux-native identity management (LDAP + Kerberos + DNS + CA). Keycloak for application-level SSO (SAML + OIDC). sssd integration with offline caching. FIDO2/passkey support (FreeIPA 4.12+)
  - Scope: Enterprise
  - Effort: 1-2 months

### Nice-to-Have Features

- **Feature:** Network segmentation (802.1X + VLAN)
  - Description: Machines auto-join correct VLAN based on role, with 802.1X authentication
  - Effort: 2-3 weeks

- **Feature:** USB device control (USBGuard)
  - Description: Per-role USB policies with violation alerting
  - Effort: 1-2 weeks

- **Feature:** Software restriction policies
  - Description: Role-based package allowlists (finance gets LibreOffice only; dev gets compilers)
  - Effort: 2 weeks

- **Feature:** Compliance dashboard
  - Description: Real-time view of fleet compliance posture across NIS2, DORA, SOC2 frameworks
  - Effort: 2-3 months

### Sovereign Cloud Integration

Enterprises subject to NIS2 or handling sensitive data increasingly need sovereign cloud providers. Key players:

| Provider | Certifications | Services | NixFleet Integration |
|----------|---------------|----------|---------------------|
| **NumSpot** | ISO 27001, SecNumCloud (in progress), HDS | IaaS, PaaS (OpenShift), AI | Binary cache hosting, control plane hosting, VM provisioning |
| **OVHcloud** | SecNumCloud (Bare Metal), ISO 27001, HDS | IaaS, Bare Metal, S3 | Binary cache on S3, dedicated servers for fleet |
| **Outscale** | SecNumCloud 3.2 | IaaS, GPU | Secure VM hosting for sensitive workloads |
| **Scaleway** | ISO 27001, HDS | IaaS, Kubernetes, S3 | Binary cache on S3, Kubernetes for control plane |
| **Clever Cloud** | ISO 27001, HDS, SecNumCloud (via Cloud Temple) | PaaS, air-gapped | PaaS deployment of NixFleet control plane |

**Strategic opportunity:** NixFleet can offer "Sovereign Fleet Management" bundles with sovereign cloud providers -- the control plane runs on NumSpot/OVHcloud, the binary cache on EU-hosted S3, and no data leaves European jurisdiction.

### ITSM/CMDB Integration Details

**ServiceNow:**
- Push CIs via ServiceNow CMDB API (Table API or Service Graph Connector)
- Map: NixOS host -> `cmdb_ci_linux_server`, NixOS generation -> `change_request`, deployment -> `change_task`
- Pull: read maintenance windows from ServiceNow to schedule deployments
- Effort: REST API integration, 1-2 months

**GLPI:**
- Push inventory via GLPI REST API (plugin FusionInventory or native)
- Map: host -> Computer, packages -> Software, config -> NetworkEquipment
- GLPI is open-source, widely used in French public sector and PMEs
- Effort: 2-3 weeks

### NIS2 Specific Implementation

NIS2 Article 21 mandates 10 specific areas. Here is how NixFleet maps to each:

| NIS2 Requirement | NixFleet Implementation | Status |
|-----------------|------------------------|--------|
| 1. Risk analysis and IS policies | Automated CIS/STIG benchmark scanning per host | Planned |
| 2. Incident handling | auditd + AIDE + fleet agent alerting + incident report templates | Planned |
| 3. Business continuity | Borg/Restic backup verification + disaster recovery runbooks | Planned |
| 4. Supply chain security | SBOM from nix closure (CycloneDX), flake input audit, lock file verification | Planned |
| 5. Network/IS acquisition security | Declarative NixOS config IS the security baseline; every change is auditable via git | Inherent |
| 6. Vulnerability assessment | vulnix CVE scanning, generation-to-generation diff | Planned |
| 7. Cybersecurity hygiene + training | Training tracking integration, security awareness module | Planned |
| 8. Cryptography policies | LUKS attestation, TLS config audit, certificate management | Partial (enterprise-features spec) |
| 9. HR security + access control | MFA enforcement, sssd/AD integration, privilege escalation logging | Partial |
| 10. MFA + secure communications | SSH hardening (done), admin MFA, encrypted channels | Partial |

**Incident reporting timeline (Article 23):**
- 24h: Early warning to CSIRT (automated alert from fleet agent)
- 72h: Detailed incident notification (auto-generated from audit logs + affected hosts)
- 1 month: Final report (generated from control plane incident timeline)

**Effort:** Full NIS2 compliance module is 3-6 months of engineering.

---

## 7. Sovereign Tier

### Persona: Government / Defense / Critical Infrastructure

**Profile:** French government ministry, military branch, nuclear operator (OIV/OSE), or critical infrastructure operator. 500-10,000+ machines. Air-gapped networks. Subject to ANSSI regulations, potential Common Criteria evaluation. Budget: EUR 100k+/year. Zero tolerance for foreign data exposure.

### The Sovereign Desktop Reality

Government offices are overwhelmingly staffed by non-technical users. The Gendarmerie Nationale's GendBuntu deployment (72,000+ machines) proved that Linux can replace Windows for office workers IF the desktop experience is polished. Key lessons:

| GendBuntu Lesson | NixFleet Implication |
|-----------------|---------------------|
| Users adapted in 2-3 weeks with training | Budget for training modules and help desk for first month |
| LibreOffice was the #1 friction point (M365 compat) | Ship with OnlyOffice or Collabora for better .docx fidelity |
| Printing was the #2 pain point | CUPS with org-managed printer list and driver packages is mandatory |
| Users didn't notice the OS change if apps looked similar | Theme the desktop to look familiar (clean, simple, not "developer-ish") |
| Help desk tickets dropped to Windows levels after 6 months | Initial spike is normal; plan for it |

### Sovereign Application Stack

For government users, EVERY application must be sovereign (no US cloud dependency):

| Need | Sovereign Solution | NixOS Package | Notes |
|------|-------------------|---------------|-------|
| Email | BlueMind, Mailu, Zimbra | `thunderbird` (client) | Server-side is separate infra |
| Office suite | LibreOffice, Collabora Online, OnlyOffice | `libreoffice` | DGFiP uses LibreOffice for 100k+ users |
| File sharing | Nextcloud (self-hosted) | `nextcloud-client` | Certified by ANSSI for some deployments |
| Video conf | Jitsi Meet, BigBlueButton | Browser-based | Self-hosted, no data to US |
| Messaging | Tchap (Matrix fork), Element | `element-desktop` | Tchap is used by French government today |
| Web browser | Firefox ESR (with sovereign policies) | `firefox-esr` | No Google sync, org-managed |
| Directory | FreeIPA | N/A (server-side) | Replaces Active Directory |
| Document mgmt | Alfresco, Nuxeo | Web-based | Legal/admin document workflows |
| Citizen kiosks | Locked Firefox in kiosk mode | `firefox` + kiosk scope | Public-facing service points |

### Certifications Required

| Certification | Issuer | Requirements | Relevance |
|--------------|--------|-------------|-----------|
| **SecNumCloud 3.2** | ANSSI (France) | 350+ requirements across 6 audit categories. EU ownership (max 24% non-EU capital). Data localization in France. ISO 27001 prerequisite. 3-year renewable | Required for any cloud service used by French government |
| **BSI C5:2025** | BSI (Germany) | Type 1 (point-in-time) and Type 2 (continuous). Covers container management, supply chain, post-quantum crypto. Mandatory for healthcare since July 2025 | Required for German government/healthcare clients |
| **Common Criteria (CC)** | International (SOG-IS in EU) | EAL2-EAL4+ depending on component. Product-level evaluation | Required for defense/intelligence systems |
| **EUCS** | ENISA (EU) | EU Cloud Services scheme (in development). Will harmonize SecNumCloud + C5 + others | Future requirement, expected 2027 |

### Air-Gap Constraints

Air-gapped deployments are the defining constraint of the Sovereign tier. Specific requirements:

| Constraint | Implication for NixFleet |
|------------|------------------------|
| No internet access | Binary cache must be on-premises. No `nix flake update` from GitHub. No SaaS anything |
| Physical media transfer | Nix closures must be exportable to USB/DVD and importable on air-gapped machines. `nix-store --export` / `--import` plus signing |
| Update cadence | Monthly or quarterly update cycles via approved media. Each update must be validated in a staging environment before transfer |
| Crypto requirements | All closures must be signed with Ed25519 keys. Chain of custody for media. Key management via HSM |
| Build provenance | Must prove that the binary was built from the declared source. Reproducible builds are critical here -- NixOS is uniquely positioned |
| No phone-home | Fleet agent must operate in pull-only mode from local server. No telemetry, no external DNS, no NTP to public servers |

**NixFleet air-gap deployment flow:**
```
Connected environment (build lab):
1. Build all closures for target fleet
2. Sign closures with org key (HSM-backed)
3. Generate SBOM + compliance attestation
4. Export to encrypted removable media
5. Chain-of-custody documentation

Air-gapped environment:
6. Import media through approved entry point
7. Verify signatures
8. Push closures to local binary cache
9. Fleet agent pulls from local cache
10. Staged deployment (canary -> full)
```

### Audit Trail Requirements

| Requirement | Implementation |
|------------|----------------|
| Every config change attributable to a person | Git commit signatures (GPG/SSH) + RBAC identity |
| Every deployment logged with before/after state | Generation diff stored in control plane DB |
| Immutable audit log | Append-only log with cryptographic chaining (similar to Certificate Transparency) |
| Retention: 5-10 years | Archived to cold storage with periodic integrity verification |
| Export format | Machine-readable (JSON/OSCAL) + human-readable (PDF reports) |
| Real-time alerting | Security-relevant events forwarded to SOC/SIEM within seconds |

### Must-Have Features (Blocking Adoption)

- **Feature:** Air-gapped closure export/import
  - Description: `nixfleet export --fleet acme --output /media/usb/` exports all closures + signatures + SBOMs. `nixfleet import --source /media/usb/` on the air-gapped side verifies and loads into local cache
  - Scope: Sovereign
  - Effort: 2-3 months

- **Feature:** Cryptographic closure signing
  - Description: Ed25519 signatures on all closures. HSM integration for key storage (PKCS#11). Signature verification before any deployment
  - Scope: Sovereign
  - Effort: 1-2 months

- **Feature:** Source code escrow
  - Description: Complete source code + build instructions deposited with a trusted third party (e.g., Iron Mountain, NCC Group). Updated with each release. Customer can rebuild from source if NixFleet ceases operations
  - Scope: Sovereign (contractual + technical)
  - Effort: 2 weeks (technical), ongoing (operational)

- **Feature:** ANSSI SecNumCloud-compatible control plane
  - Description: Control plane that meets SecNumCloud 3.2 requirements: hosted in France, EU ownership, data localization, ISO 27001 processes. Partnership with a SecNumCloud-qualified provider (OVHcloud, Outscale, NumSpot)
  - Scope: Sovereign
  - Effort: 6-12 months (includes certification process)

- **Feature:** Reproducible build attestation
  - Description: Prove that every deployed binary matches its source. NixOS is one of the few systems where this is feasible. Generate build attestation documents (SLSA Level 3+) for each closure
  - Scope: Sovereign
  - Effort: 2-3 months

- **Feature:** OSCAL compliance output
  - Description: Auto-generate compliance documentation in OSCAL (Open Security Controls Assessment Language) format for automated compliance checking by auditors
  - Scope: Sovereign
  - Effort: 1-2 months

- **Feature:** On-site deployment support
  - Description: NixFleet engineers on-site for initial deployment, training, and first update cycle. Required by most government contracts
  - Scope: Sovereign (services)
  - Effort: Operational (staffing)

- **Feature:** ANSSI PA-114 compliant workstation hardening
  - Description: Implement ANSSI's January 2026 recommendations for multi-environment workstation security: secure boot, no local admin, USB restrictions, session locking, EDR, rapid patching. Map each ANSSI recommendation to a NixOS module
  - Scope: Sovereign
  - Effort: 2-3 months

### Nice-to-Have Features

- **Feature:** Post-quantum cryptography readiness
  - Description: BSI C5:2025 addresses post-quantum crypto. Prepare NixOS modules for hybrid key exchange (X25519 + ML-KEM)
  - Effort: Research phase, 2-4 weeks

- **Feature:** Hardware fingerprinting
  - Description: Cryptographic binding of config to specific hardware (TPM attestation). Detect if a disk is moved to unauthorized hardware
  - Effort: 1-2 months

- **Feature:** Multi-classification support
  - Description: Different security levels on the same machine (unclassified + restricted) via virtualization or containers
  - Effort: 3-6 months (complex, may require certification)

---

## 8. Windows-to-NixOS Migration Guide

### 8.1 The Migration Challenge

For Pro and Enterprise tiers, the primary migration is FROM Windows + Microsoft 365, not from Ansible or Docker. The majority of users being migrated are office workers who have used Windows their entire career. This is a USER migration, not just a SYSTEM migration.

### 8.2 What Changes for End Users

| Windows + M365 | NixOS + Sovereign Stack | User Impact |
|----------------|------------------------|-------------|
| Outlook | Thunderbird + TbSync (or Evolution) | Medium -- different UI, same IMAP/CalDAV protocols |
| Word/Excel/PowerPoint | LibreOffice or OnlyOffice | High -- formatting differences in complex documents |
| OneDrive | Nextcloud client or kDrive | Low -- same concept, different client |
| Teams | Jitsi Meet or BigBlueButton | Medium -- browser-based, no desktop client polish |
| SharePoint | Nextcloud or Alfresco | Medium -- different workflow for document collaboration |
| Windows Explorer | Nautilus/Thunar with SMB mounts | Low -- file management is file management |
| Windows + AD login | NixOS + FreeIPA/Keycloak | Low -- same username/password, different boot screen |
| Ctrl+Alt+Del -> Lock | Super+L or auto-lock | Low -- just a different shortcut |
| Windows Update | Transparent (fleet agent handles it) | Positive -- no more reboot-during-meeting surprises |
| Group Policy | NixOS declarative config | Invisible to users (IT team concern only) |

### 8.3 Training Needs by Profile

| Profile | Training Duration | Key Topics | Format |
|---------|------------------|------------|--------|
| Office worker | 2 hours + 1 week buddy system | LibreOffice basics, email setup, file sharing, printing | In-person workshop |
| Manager | 1 hour | Same as office + video conferencing, screen sharing | 1-on-1 with IT |
| Accounting | 4 hours | RDP access to Windows apps (Sage), web-based alternatives | Hands-on with IT |
| IT team | 2 days | NixOS fundamentals, NixFleet dashboard, troubleshooting, role management | Technical training |
| Reception / kiosk | 30 min | "Click this icon to do X" -- kiosk mode is simpler than Windows | Quick walkthrough |

### 8.4 Hybrid Coexistence Strategy

No organization migrates 100% at once. NixFleet must support a MIXED fleet during transition:

**Phase 0 -- Preparation (1-2 months before migration)**
- Deploy NixFleet alongside existing Windows management (SCCM/Intune)
- Set up FreeIPA with AD trust (users authenticate against both)
- Configure Samba file shares accessible from both Windows and NixOS
- Deploy sovereign email (dual delivery: M365 + sovereign for pilot group)
- Package Windows-only apps for RDP access from NixOS

**Phase 1 -- Pilot (2-3 months)**
- Migrate 10-20 low-risk machines (reception, shared spaces, IT team)
- Keep Windows available via RDP for fallback
- Measure: help desk tickets, user satisfaction, printer issues
- Document every friction point and resolve before Phase 2

**Phase 2 -- Department rollout (3-6 months)**
- One department at a time, starting with lowest Windows dependency
- Typical order: IT -> Marketing -> Sales -> HR -> Legal -> Finance
- Finance is LAST because of Sage/SAP/banking app dependencies
- Each department gets 1 week of training + 2 weeks of intensive support

**Phase 3 -- Full migration (6-12 months)**
- Remaining Windows machines converted or kept for specific apps only
- AD -> FreeIPA migration completed (or AD trust maintained for remaining Windows)
- M365 subscriptions reduced to minimum (or eliminated)

**Phase 4 -- Windows residual (ongoing)**
- Some machines may remain Windows for years (specialized hardware, certified software)
- NixFleet manages the NixOS fleet; Windows machines remain on SCCM/Intune
- Goal: reduce Windows to <10% of fleet over 2 years

### 8.5 Critical Success Factors

| Factor | Requirement | NixFleet Feature |
|--------|-------------|-----------------|
| **Document compatibility** | .docx/.xlsx must round-trip without corruption | OnlyOffice or Collabora (better compat than LibreOffice) |
| **Printing works Day 1** | Every printer the user had on Windows works on NixOS | Org-level printer list with drivers in fleet config |
| **Email works Day 1** | Inbox, calendar, contacts migrated before user sits down | Thunderbird autoconfig + IMAP migration script |
| **Same password** | Users log in with their existing AD password | FreeIPA-AD trust or LDAP proxy |
| **Help desk ready** | IT team can answer NixOS questions from Day 1 | Training + runbooks + NixFleet dashboard |
| **Fallback available** | If something is broken, user can RDP to a Windows VM | RDP client pre-configured, Windows terminal servers available |
| **Executive sponsorship** | CTO/CEO visibly using NixOS | Executive role with premium setup (large screen, good video conf) |

### 8.6 Cost Comparison (50-Person PME)

| Item | Windows + M365 | NixOS + Sovereign Stack | Savings |
|------|---------------|------------------------|---------|
| M365 Business Standard | 50 x EUR 12.50/month = EUR 7,500/year | EUR 0 (sovereign email) | EUR 7,500/year |
| Windows licenses (per seat) | 50 x EUR 150 = EUR 7,500 (one-time, per upgrade) | EUR 0 | EUR 7,500/cycle |
| SCCM/Intune | EUR 3,000-10,000/year | NixFleet Pro: EUR 6,000-36,000/year | Variable |
| Antivirus | 50 x EUR 30/year = EUR 1,500/year | Open-source (ClamAV, Wazuh) | EUR 1,500/year |
| Training | EUR 0 (users know Windows) | EUR 5,000-10,000 (one-time) | -EUR 5,000-10,000 |
| **Total Year 1** | **~EUR 19,500 + licenses** | **~EUR 11,000-46,000** | Depends on tier |
| **Total Year 2+** | **~EUR 12,000/year** | **~EUR 6,000-36,000/year** | Sovereignty gains |

**Note:** The financial case is weaker for small PMEs. The primary driver is sovereignty, compliance, and long-term independence from Microsoft licensing changes. Cost savings become significant at 200+ seats.

---

## 9. Acceptance Criteria for Production-Ready

### What a CTO Needs to See

| Criterion | Community | Pro | Enterprise | Sovereign |
|-----------|-----------|-----|------------|-----------|
| Works on >2 real machines | Required | Required | Required | Required |
| Documentation quality | Good README | Full docs site | Full docs + architecture review | Full docs + security review |
| Deployment mechanism | CLI push | CLI + dashboard | Staged pipeline + approval | Air-gap + signed |
| Rollback capability | Manual | One-click | Automated on failure | Automated + audit trail |
| Uptime guarantee | Best-effort | 99.5% | 99.9% SLA | 99.99% SLA |
| Support | Community | Email (24h) | Dedicated (4h P1) | On-site + dedicated |
| Reference customers | GitHub stars | 2-3 testimonials | Named references | Government references |
| Security audit | Self-assessed | Annual pentest | Annual pentest + SOC2 | ANSSI certification |

### What a CISO Needs to See

| Criterion | Minimum for Approval |
|-----------|---------------------|
| Vulnerability management | CVE scanning with SLA for critical patches (24h for CVSS 9+) |
| Access control | MFA for all admin access, RBAC with least-privilege |
| Audit trail | Immutable, exportable, retained for 5+ years |
| Incident response | Documented IR plan, tested annually, integrated with fleet agent |
| Supply chain | SBOM for every deployment, signed artifacts, dependency audit |
| Encryption | At-rest (LUKS) + in-transit (TLS 1.3) + admin access (SSH with certificates) |
| Compliance mapping | Documented mapping to NIS2/DORA/SOC2 controls |
| Third-party assessment | Independent security audit (pentest + architecture review) |
| Data residency | Proof that no data leaves EU (for Pro+) or France (for Sovereign) |
| Business continuity | RTO < 4h, RPO < 1h, tested disaster recovery |

### Competitive Positioning Summary

| Capability | NixFleet | Fleet.dm | Puppet | Ansible | SCCM/Intune |
|------------|----------|----------|--------|---------|-------------|
| Reproducible config | Native (Nix) | No | Partial | No | No |
| Drift detection | Inherent (declarative) | osquery-based | Agent-based | Manual | Agent-based |
| Linux-native | Yes | Yes | Yes | Yes | Limited |
| Air-gap capable | Planned | Yes | Yes | Yes | No |
| GitOps/IaC | Native | Yes (YAML) | Partial (Bolt) | Yes (playbooks) | No |
| EU sovereignty | Core value | No (US company) | No (Perforce, US) | No (Red Hat/IBM, US) | No (Microsoft, US) |
| Open-source core | Apache 2.0 | Apache 2.0 | Deprecated OSS | Apache 2.0 | Proprietary |
| NIS2 compliance | Planned | No | Partial | No | Partial |
| Binary reproducibility | Native (Nix) | No | No | No | No |
| Rollback | Native (generations) | No | No | No | Limited |

**NixFleet's unique advantages:**
1. **Reproducibility** -- the only fleet tool where config = state, guaranteed
2. **EU sovereignty** -- European company, EU-hosted, GDPR-native
3. **Rollback** -- NixOS generations provide instant, reliable rollback that no competitor matches
4. **Supply chain security** -- Nix closures provide complete dependency graphs, enabling true SBOMs

**NixFleet's challenges:**
1. **Nix learning curve** -- steeper than Ansible/Puppet YAML
2. **Ecosystem maturity** -- smaller community than Ansible (31.7% market share)
3. **Windows gap** -- cannot manage Windows endpoints (unlike Fleet.dm, SCCM, Intune)
4. **No existing enterprise references** -- must build credibility from scratch

### Priority Roadmap for Production-Readiness

**Phase 1 (0-3 months): Community tier viable**
1. Fleet deployment CLI (wrap Colmena or build native)
2. `nixfleet init` wizard
3. Basic `nixfleet status` monitoring
4. Binary cache setup automation
5. Documentation + migration guides

**Phase 2 (3-6 months): Pro tier viable**
1. Dashboard UI (fleet inventory, generation history)
2. RBAC + audit logging
3. Managed binary cache (EU-hosted Attic)
4. LDAP/AD integration
5. GLPI integration
6. Enterprise scope modules (VPN, printing, file sharing)

**Phase 3 (6-12 months): Enterprise tier viable**
1. SSO/SAML (Keycloak integration)
2. NIS2 compliance module (SBOM, CVE scan, incident templates)
3. ServiceNow CMDB integration
4. Staged deployment pipelines
5. On-premises control plane
6. EDR integration (Wazuh + CrowdStrike packaging)

**Phase 4 (12-18 months): Sovereign tier viable**
1. Air-gapped deployment + signed closures
2. SecNumCloud certification (via partner)
3. OSCAL compliance output
4. Source code escrow
5. ANSSI PA-114 workstation hardening modules
6. On-site deployment capability

---

## Sources

### KSuite / Infomaniak
- [Avis Infomaniak kSuite (2026) - Clubic](https://www.clubic.com/outils-productivite/avis-414744-avis-infomaniak-kdrive-2022.html)
- [kSuite - The ethical and secure collaborative solution](https://www.infomaniak.com/en/ksuite)
- [kSuite - Wikipedia](https://en.wikipedia.org/wiki/KSuite)
- [kSuite Pro - Infomaniak](https://www.infomaniak.com/en/ksuite/ksuite-pro)

### NIS2 Directive
- [NIS2 Article 21: Cybersecurity risk-management measures](https://www.nis-2-directive.com/NIS_2_Directive_Article_21.html)
- [NIS2 Article 21 Risk Management Measures Explained](https://www.glocertinternational.com/resources/guides/nis2-article-21-risk-management-measures-explained/)
- [NIS2 Technical Implementation Guidance - ENISA](https://www.enisa.europa.eu/publications/nis2-technical-implementation-guidance)
- [NIS2 requirements: A complete guide - DataGuard](https://www.dataguard.com/nis2/requirements/)
- [NIS2 Starts with Securely Managed Endpoints - KuppingerCole](https://www.kuppingercole.com/research/wp81008/nis2-starts-with-securely-managed-endpoints)
- [NIS2 Article 23: Reporting obligations](https://www.nis-2-directive.com/NIS_2_Directive_Article_23.html)

### French PME IT
- [ERP Industrial Software Landscape in France 2026](https://xavierminali.com/en/executive-insights-blog-digital-transformation/industrial-software-landscape-erp-in-france-in-2026)
- [France IT Services Market Size - Mordor Intelligence](https://www.mordorintelligence.com/industry-reports/france-it-services-market)

### Sovereign Cloud
- [NumSpot - Sovereign Cloud IDC One Pager (Red Hat)](https://www.redhat.com/tracks/_pfcdn/assets/10330/contents/1095226/c205d301-225e-4f31-875b-d1d412853db2.pdf)
- [NumSpot - La Poste Groupe](https://www.lapostegroupe.com/en/news/numspot-is-rethinking-cloud-management-with-an-innovative-next-generation-platform)
- [7 Certified French Cloud Solutions 2025 - Drime](https://drime.cloud/blog-posts/7-certified-french-cloud-solutions-for-your-business-in-2025)
- [SecNumCloud Qualification Guide - Scalingo](https://scalingo.com/blog/secnumcloud-qualification-anssi-guide)
- [SecNumCloud Guide - FeelAgile](https://www.feelagile.com/en/guide/guide-secnumcloud)
- [OVHcloud SecNumCloud](https://www.ovhcloud.com/en/compliance/secnumcloud/)

### ANSSI Recommendations
- [ANSSI-PA-114: Securisation du poste de travail multi-environnements](https://messervices.cyber.gouv.fr/documents-guides/anssi-fondamentaux-securisation-poste-multi-environnements-v1-0.pdf)
- [CNIL: Securiser les postes de travail](https://www.cnil.fr/fr/securite-securiser-les-postes-de-travail)
- [ANSSI Recommandations administration securisee SI](https://messervices.cyber.gouv.fr/documents-guides/anssi-guide-admin_securisee_si_v3-0.pdf)

### Competitors
- [Fleet - Redefining endpoint management at scale (2026 Gartner)](https://fleetdm.com/announcements/redefining-endpoint-management-at-scale)
- [Fleet - Open device management](https://fleetdm.com/)
- [Fleet - Infrastructure as code](https://fleetdm.com/fleet-gitops)
- [Ansible vs Puppet - Puppet blog](https://www.puppet.com/blog/ansible-vs-puppet)
- [Chef vs Puppet vs Ansible 2026 - Better Stack](https://betterstack.com/community/comparisons/chef-vs-puppet-vs-ansible/)
- [Colmena - GitHub](https://github.com/zhaofengli/colmena)

### BSI C5
- [BSI C5 Explained - Kiteworks](https://www.kiteworks.com/regulatory-compliance/bsi-c5-germanys-cloud-security-framework-requirements/)
- [BSI C5:2025 Community Draft](https://www.bsi.bund.de/EN/Themen/Unternehmen-und-Organisationen/Informationen-und-Empfehlungen/Empfehlungen-nach-Angriffszielen/Cloud-Computing/Kriterienkatalog-C5/C5_2025/C5_2025_node.html)

### Linux Desktop Enterprise Adoption
- [GendBuntu - France's Linux Migration](https://medium.com/@majdidraouil/the-end-of-windows-how-france-s-gendbuntu-signals-a-shift-from-costly-patch-plagued-systems-2086aee86fe9)
- [Why 2026 might bring more Linux desktops to the enterprise - TechTarget](https://www.techtarget.com/searchenterprisedesktop/feature/Why-2026-might-bring-more-Linux-desktops-to-the-enterprise)
- [EU-Linux petition and policy](https://licenseware.io/from-petition-to-policy-how-europes-call-for-eu-linux-signals-a-continental-shift-away-from-big-tech-dependency/)

### Directory Services
- [FreeIPA vs Keycloak - StackShare](https://stackshare.io/stackups/freeipa-vs-keycloak)
- [FreeIPA and Keycloak Integration Guide](https://copyprogramming.com/howto/net-core-2-1-linux-keycloak-integration-authentication-openid-connect-sssd)

### EDR / Endpoint Security
- [OpenEDR - Open Source EDR](https://www.openedr.com/)
- [Top EDR Tools 2026 - CyberNX](https://www.cybernx.com/edr-tools/)

### ITSM Integration
- [ServiceNow CMDB Integration - The Cloud People](https://www.thecloudpeople.com/blog/unleashing-the-power-of-cmdb-integration-with-servicenow)
- [GLPI Alternatives 2026 - GoWorkWize](https://www.goworkwize.com/blog/glpi-alternatives)
