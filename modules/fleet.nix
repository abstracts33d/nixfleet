# Minimal test fleet for the NixFleet framework repo.
# These hosts exist to make eval tests pass — they are NOT a real org fleet.
# No secrets, no agenix, no real hardware.
{config, ...}: let
  inherit (config.nixfleet.lib) mkFleet mkOrg mkHost mkBatchHosts mkTestMatrix builtinRoles;

  # -- Test organization (mirrors abstracts33d defaults for eval test compatibility) --
  abstracts33d = mkOrg {
    name = "abstracts33d";
    description = "Framework test organization";
    hostSpecDefaults = {
      userName = "s33d";
      githubUser = "abstracts33d";
      githubEmail = "abstract.s33d@gmail.com";
      timeZone = "Europe/Paris";
      locale = "en_US.UTF-8";
      keyboardLayout = "us";
      gpgSigningKey = "77C21CC574933465";
      sshAuthorizedKeys = [
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIB+qnpVT15QebM41WFgwktTMP6W/KXymb8gxNV0bu5dw"
      ];
      theme = {
        flavor = "macchiato";
        accent = "lavender";
      };
      hashedPasswordFile = "/run/agenix/user-password";
      rootHashedPasswordFile = "/run/agenix/root-password";
    };
    # No nixosModules — framework repo has no agenix/secrets input
  };

  fleet = mkFleet {
    organizations = [abstracts33d];
    hosts =
      [
        # krach: isDev=true (default), used for org defaults / password / GPG / SSH tests
        (mkHost {
          hostName = "krach";
          org = abstracts33d;
          platform = "x86_64-linux";
          isVm = true;
          hostSpecValues = {
            hostName = "krach";
            isImpermanent = true;
            useNiri = true;
          };
        })

        # krach-qemu: useNiri + isImpermanent, scope activation tests
        (mkHost {
          hostName = "krach-qemu";
          org = abstracts33d;
          platform = "x86_64-linux";
          isVm = true;
          hostSpecValues = {
            hostName = "krach-qemu";
            isImpermanent = true;
            useNiri = true;
            isDev = false;
          };
        })

        # ohm: useGnome, userName override
        (mkHost {
          hostName = "ohm";
          org = abstracts33d;
          platform = "x86_64-linux";
          isVm = true;
          hostSpecValues = {
            hostName = "ohm";
            userName = "sabrina";
            useGnome = true;
            isDev = false;
          };
        })

        # qemu: isMinimal
        (mkHost {
          hostName = "qemu";
          org = abstracts33d;
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
          org = abstracts33d;
          platform = "x86_64-linux";
          isVm = true;
          hostSpecValues = {
            hostName = "lab";
            isServer = true;
            isDev = false;
            isGraphical = false;
          };
        })
      ]
      # Batch hosts (edge fleet)
      ++ (mkBatchHosts {
        template = {
          org = abstracts33d;
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
        org = abstracts33d;
        roles = with builtinRoles; [workstation server minimal];
        platforms = ["x86_64-linux"];
      });
  };
in {
  flake = {
    inherit (fleet) nixosConfigurations;
  };
}
