# Shared: builds the JSON payload for /etc/nixfleet/{agent,cp}/trust.json
# from config.nixfleet.trust. Shape must match crates/nixfleet-proto's
# TrustConfig — see crates/nixfleet-proto/src/trust.rs and
# docs/trust-root-flow.md §3.4.
#
# `ciReleaseKey` is already in proto shape on the option side (typed
# {algorithm, public} submodules per CONTRACTS §II #1) and passes
# through unchanged.
#
# `cacheKeys` is a flat list of opaque trusted-key strings forwarded
# verbatim to nix's `trusted-public-keys`. The framework doesn't
# parse these — fleets pick whatever cache implementation they want
# (harmonia, attic, cachix, ...) and supply its native key format.
#
# `orgRootKey` stores bare-string key material on the option side
# (keySlotType in modules/contracts/trust.nix), pinned to ed25519 per
# CONTRACTS §II #3. This helper promotes it into typed TrustedPubkey
# entries matching proto's KeySlot shape.
{trust}: let
  wrapEd25519 = key:
    if key == null
    then null
    else {
      algorithm = "ed25519";
      public = key;
    };
in {
  schemaVersion = 1;
  ciReleaseKey = trust.ciReleaseKey;
  cacheKeys = trust.cacheKeys;
  orgRootKey = {
    current = wrapEd25519 trust.orgRootKey.current;
    previous = wrapEd25519 trust.orgRootKey.previous;
    rejectBefore = trust.orgRootKey.rejectBefore;
  };
}
