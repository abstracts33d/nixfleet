# `nixfleet_cli::bin::mint_token`

`nixfleet-mint-token` — operator-side bootstrap token minter.

Phase 3 PR-5. Run once on the operator's workstation per new
fleet host (typically as part of declaring the host in
fleet.nix and committing an agenix-encrypted token).

Usage:

```text
nixfleet-mint-token \
    --hostname krach \
    --csr-pubkey-fingerprint <sha256-base64-of-CSR-spki> \
    --org-root-key /path/to/org-root.ed25519.key \
    --validity-hours 24 \
    > bootstrap-token-krach.json
```

The agent's first-boot enrollment generates its own keypair
before posting the CSR; in practice the operator runs
`nixfleet-mint-token` AFTER the host has booted and produced
its CSR (typically captured by the deploy tooling). For an even
simpler workflow, omit `--csr-pubkey-fingerprint` and accept any
pubkey — but that weakens the binding (a leaked token can be
used with an attacker-controlled key). Default keeps the
fingerprint required.

