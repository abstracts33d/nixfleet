# Enterprise scope: Network printing
# CUPS + network printer auto-discovery + org-managed printer list
# Phase 2+: reads printer list from org defaults
{...}: {
  flake.modules.nixos.enterprisePrinting = {
    config,
    lib,
    ...
  }: let
    hS = config.hostSpec;
  in {
    config = lib.mkIf hS.usePrinting {
      # CUPS printing service
      services.printing = {
        enable = true;
        # TODO: org-managed printer list via hardware.printers.ensurePrinters
        # TODO: PPD driver packages per org (gutenprint, hplip, etc.)
        # TODO: default printer per location/role
        # TODO: print quota integration (pykota or similar)
      };

      # Avahi for network printer auto-discovery
      services.avahi = {
        enable = true;
        nssmdns4 = true;
        openFirewall = true;
      };

      # TODO: org-level printer definitions deployed declaratively
      # hardware.printers.ensurePrinters = [ ... ];
      # hardware.printers.ensureDefaultPrinter = "...";
    };
  };
}
