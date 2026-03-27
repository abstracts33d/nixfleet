# NIS2 Compliance

Distilled knowledge about NIS2 and how NixOS satisfies it.

## What NIS2 Is

EU Directive 2022/2555 replacing NIS1. Massively expands scope: from ~300 entities (NIS1) to 15,000+ (NIS2) in France alone. Compliance required by end of 2027. Fines up to EUR 10M or 2% global revenue, with personal liability for leadership (Article 20).

## NixFleet's Thesis

NixOS satisfies most NIS2 obligations **by construction**, turning compliance from a cost center into a product feature.

## Article 21: The 10 Minimum Measures

| # | Measure | NixOS Native? | NixFleet Addition |
|---|---------|--------------|-------------------|
| 1 | Risk analysis and IS security policies | Partial | Compliance dashboard, policy-as-code evidence |
| 2 | Incident handling | Partial | Incident timeline viewer, state reconstruction |
| 3 | Business continuity | Yes (atomic rollback <90s) | Automated DR drill reports |
| 4 | Supply chain security | Yes (flake.lock pins by hash) | SBOM generation, provenance reports |
| 5 | Security in development/maintenance | Partial | vulnix integration, CVE alerts |
| 6 | Vulnerability handling | Partial | CVE scanning against SBOM |
| 7 | Effectiveness assessment | No | Compliance score over time, drift detection |
| 8 | Cyber hygiene and training | No (organizational) | Training module tracking (out of scope) |
| 9 | Cryptography and encryption | Partial (LUKS, agenix, TLS) | Encryption audit, key rotation tracking |
| 10 | Access control and asset management | Partial | Asset inventory from nixosConfigurations |

## What Comes Free with NixOS

- **Traceability**: Every config change is a git commit (tamper-evident audit trail)
- **Incident recovery**: Generations are immutable snapshots; rollback is atomic
- **Supply chain**: `flake.lock` pins every input by content hash (SHA-256)
- **Asset inventory**: `nixosConfigurations` IS the asset inventory (no CMDB drift)

## What Needs Tooling (NixFleet value-add)

1. **SBOM generation** -- translate flake.lock + closure into CycloneDX/SPDX
2. **Vulnerability scanning** -- match SBOM against NVD/CVE databases
3. **Incident timeline** -- reconstruct machine state at any historical point
4. **Compliance score** -- quantified measure of NIS2 coverage
5. **Exportable reports** -- regulator-ready compliance documentation

## Transposition Status (March 2026)

- **France**: Loi Resilience adopted by Senate (March 2025), finalizing
- **Germany**: NIS2UmsuCG in legislative process
- Deadline: end 2027 across EU
