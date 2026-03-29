{lib, ...}: {
  options.flake.modules = {
    nixos = lib.mkOption {
      type = lib.types.attrsOf lib.types.deferredModule;
      default = {};
      description = "NixOS deferred modules composable by hosts";
    };
    darwin = lib.mkOption {
      type = lib.types.attrsOf lib.types.deferredModule;
      default = {};
      description = "Darwin deferred modules composable by hosts";
    };
    homeManager = lib.mkOption {
      type = lib.types.attrsOf lib.types.deferredModule;
      default = {};
      description = "Home-manager deferred modules composable by hosts";
    };
    hmLinux = lib.mkOption {
      type = lib.types.attrsOf lib.types.deferredModule;
      default = {};
      description = "Home-manager deferred modules for Linux hosts only";
    };
    hmDarwin = lib.mkOption {
      type = lib.types.attrsOf lib.types.deferredModule;
      default = {};
      description = "Home-manager deferred modules for Darwin hosts only";
    };
  };
}
