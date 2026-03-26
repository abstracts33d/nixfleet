# Minimal test fleet for the NixFleet framework repo.
# These hosts exist to make eval tests pass — they are NOT a real org fleet.
# No secrets, no agenix, no real hardware.
# Fleet-specific hostSpec options (isDev, isGraphical, useNiri, theme, etc.)
# are NOT available here — those are declared by consuming fleets.
{config, ...}: let
  inherit (config.nixfleet.lib) mkFleet mkOrg mkHost mkBatchHosts mkTestMatrix builtinRoles;

  # -- Test organization (generic defaults for framework eval tests) --
  testOrg = mkOrg {
    name = "test-org";
    description = "Framework test organization";
    hostSpecDefaults = {
      userName = "testuser";
      timeZone = "UTC";
      locale = "en_US.UTF-8";
      keyboardLayout = "us";
      sshAuthorizedKeys = [
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAINixfleetTestKeyDoNotUseInProduction"
      ];
    };
    # No nixosModules — framework repo has no agenix/secrets input
  };

  fleet = mkFleet {
    organizations = [testOrg];
    hosts =
      [
        # krach: used for org defaults / password / SSH tests
        (mkHost {
          hostName = "krach";
          org = testOrg;
          platform = "x86_64-linux";
          isVm = true;
          hostSpecValues = {
            hostName = "krach";
            isImpermanent = true;
          };
        })

        # krach-qemu: scope activation tests
        (mkHost {
          hostName = "krach-qemu";
          org = testOrg;
          platform = "x86_64-linux";
          isVm = true;
          hostSpecValues = {
            hostName = "krach-qemu";
            isImpermanent = true;
          };
        })

        # ohm: userName override
        (mkHost {
          hostName = "ohm";
          org = testOrg;
          platform = "x86_64-linux";
          isVm = true;
          hostSpecValues = {
            hostName = "ohm";
            userName = "sabrina";
          };
        })

        # qemu: isMinimal
        (mkHost {
          hostName = "qemu";
          org = testOrg;
          platform = "x86_64-linux";
          isVm = true;
          hostSpecValues = {
            hostName = "qemu";
            isMinimal = true;
          };
        })

        # lab: server host
        (mkHost {
          hostName = "lab";
          org = testOrg;
          platform = "x86_64-linux";
          isVm = true;
          hostSpecValues = {
            hostName = "lab";
            isServer = true;
          };
        })
      ]
      # Batch hosts (edge fleet)
      ++ (mkBatchHosts {
        template = {
          org = testOrg;
          role = builtinRoles.edge;
          platform = "x86_64-linux";
          isVm = true;
        };
        instances = [
          {hostName = "edge-01";}
          {hostName = "edge-02";}
          {hostName = "edge-03";}
        ];
      })
      # Test matrix (validates all roles evaluate correctly)
      ++ (mkTestMatrix {
        org = testOrg;
        roles = with builtinRoles; [workstation server minimal];
        platforms = ["x86_64-linux"];
      });
  };
in {
  flake = {
    inherit (fleet) nixosConfigurations;
  };
}
