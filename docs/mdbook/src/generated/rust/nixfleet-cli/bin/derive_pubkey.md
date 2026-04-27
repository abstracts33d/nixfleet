# `nixfleet_cli::bin::derive_pubkey`

Tiny operator-workstation helper: read raw ed25519 private key
bytes from a file, print base64-encoded public key. Used once
per fleet-life to derive what goes into
`nixfleet.trust.orgRootKey.current` in fleet.nix.

This isn't shipped with the CP — it's a build-time scratch binary
the operator runs locally. Lives next to `mint_token.rs` so it
shares the workspace's ed25519-dalek + base64 deps.

