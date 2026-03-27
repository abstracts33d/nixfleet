---
name: incident
description: Incident response — diagnose fleet issue, assess security impact, recommend fix.
user-invocable: true
argument-hint: "<description of the issue>"
---

# Incident Response

## Input

The user provides a description of the issue. If not provided, ask:
- "What is the symptom? (build failure, service down, config drift, security alert, etc.)"
- "Which host(s) are affected?"
- "When did it start?"

## Process

### Stage 1 — Fleet Assessment (fleet-ops)

Dispatch `fleet-ops` with the incident description:
- Identify affected hosts from the description and fleet inventory
- Determine blast radius: is this one host, one role, or org-wide?
- Check recent deployments: `git log --oneline -10` for config changes near incident time
- Check CI status: `gh run list --limit 5` for recent build failures
- Assess whether a rollback is immediately needed
- Output: affected hosts list + blast radius + recent change log + rollback recommendation

### Stage 2 — Root Cause Diagnosis (nix-expert)

Dispatch `nix-expert` with the Stage 1 output:
- Diagnose the root cause:
  - Build error? → trace the failing derivation
  - Config drift? → compare expected vs actual state
  - Broken module? → identify the module and its import chain
  - Service failure? → check systemd unit definition and activation script
  - Impermanence issue? → verify persist paths and bind mounts
- Reference `.claude/rules/nix-gotchas.md` for known pitfalls
- Propose a minimal fix (not a structural overhaul — that comes next)
- Output: root cause analysis + minimal fix + affected files

### Stage 3 — Security Impact (security-reviewer)

Dispatch `security-reviewer` with Stage 1+2 output:
- Was any secret or credential exposed by this incident?
- Did a permission boundary fail?
- Is there a supply chain concern (upstream input, dependency)?
- Assess: is this an isolated failure or a systemic vulnerability?
- If High/Critical findings: write timestamped report to `.claude/security-reviews/YYYY-MM-DD-incident.md`
- Output: security impact assessment + report path (if written)

### Stage 4 — Structural Fix (architect)

Dispatch `architect` with all Stage 1–3 output:
- Is the root cause a symptom of a deeper structural issue?
- Recommend the proper fix (not just the minimal patch from Stage 2):
  - Refactor the affected module?
  - Add a missing eval test to catch this class of error?
  - Change a hostSpec default?
  - Add impermanence guard?
- Define a rollback plan if the structural fix cannot be applied immediately
- Output: structural recommendation + rollback plan + test coverage gap

### Stage 5 — Present

```
## Incident Report — YYYY-MM-DD HH:MM

### Symptom
<user description>

### Affected Hosts
<list from fleet-ops>

### Root Cause
<diagnosis from nix-expert>

### Security Impact
Severity: [None / Low / Medium / High / Critical]
<summary from security-reviewer>

### Fix

#### Immediate (minimal patch)
<from nix-expert>

#### Structural (proper fix)
<from architect>

### Rollback Plan
<from architect>

### Test Gap
<eval or VM test to add to prevent recurrence>

### Files to Change
<list>
```

## Chaining

After presenting:
- If immediate fix is needed: dispatch `nix-expert` to implement the patch, then `/review`
- If structural fix is needed: invoke `/feature` with the structural fix as input
- If security report was written: invoke `/security` to track remediation

## Verification

Before presenting, invoke `superpowers:verification-before-completion`:
- Show actual git log output used for timeline
- Show that security report file exists if written
- Never claim "root cause found" without a concrete trace or evidence
