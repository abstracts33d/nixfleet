# Test matrix generator. Creates one VM host per role×platform combination.
# Used for CI validation that all roles build correctly.
{lib}: let
  mkHost = import ./mk-host.nix;

  # Convert platform string to a clean hostname suffix
  # "x86_64-linux" -> "x86-64"
  # "aarch64-linux" -> "aarch64"
  # "aarch64-darwin" -> "aarch64-darwin"
  platformSuffix = platform: let
    parts = lib.splitString "-" platform;
  in
    builtins.head parts;
in
  {
    org,
    roles,
    platforms ? ["x86_64-linux"],
    namePrefix ? "test",
    ...
  }:
    lib.concatMap (
      role:
        map (
          platform:
            mkHost {
              hostName = "${namePrefix}-${role.name}-${platformSuffix platform}";
              inherit org role platform;
              isVm = true;
            }
        )
        platforms
    )
    roles
