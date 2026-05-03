# LOADBEARING: shape must match proto::TrustConfig (consumed at runtime by agent + CP).
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
