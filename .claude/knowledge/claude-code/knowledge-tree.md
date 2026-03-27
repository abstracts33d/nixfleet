# Knowledge Tree Architecture

## Purpose

Replace monolithic CLAUDE.md with scoped, hierarchical knowledge that agents consume selectively.

## Structure

```
.claude/knowledge/
  nix/                  # Nix module system, gotchas, flake-parts
  rust/                 # Agent/control-plane patterns, cargo, testing
  nixfleet/             # Product: API, architecture, tiers, decisions
  operations/           # Deploy, secrets, impermanence
  security/             # Hardening, reviews, permissions
  languages/            # Nix, Rust, Go, Bash specifics
  claude-code/          # THIS directory — meta knowledge about the config itself
  hardware/             # QEMU, UTM, SPICE, hardware-configuration
  _proposals/           # Agent-proposed additions (pending review)
```

## Writing Knowledge

- Each file is a focused, scannable document (not a dump)
- Use tables and code blocks for quick reference
- Include "why" not just "what" — context matters for agents
- Link to source files when referencing specific code

## Proposal Flow

1. Agent discovers pattern/gotcha during work
2. Agent writes to `_proposals/<agent>-<topic>-<date>.md`
3. config-manager agent reviews proposals periodically
4. Human approves integration into the correct domain file
5. Proposal file deleted after integration

## Maintenance

The config-manager agent runs a health check:
- Proposals pending? → integrate or discard
- Knowledge files stale? → update from current code
- CLAUDE.md over 200 lines? → extract knowledge
- Agent scope drift? → update knowledge: fields
