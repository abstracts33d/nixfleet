# TPM-backed signing keyslot. First-boot oneshot creates a primary,
# evicts to a persistent handle, exports the pubkey. Idempotent across
# impermanence wipes — re-extracts from the persisted handle.
{
  config,
  lib,
  pkgs,
  ...
}: let
  cfg = config.nixfleet.keyslots.tpm;
  pubkeyPem = "${cfg.exportPubkeyDir}/pubkey.pem";
  pubkeyRaw = "${cfg.exportPubkeyDir}/pubkey.raw";

  algo =
    {
      "ecdsa-p256" = {
        createPrimaryArgs = "--key-algorithm ecc256:ecdsasha256";
        # LOADBEARING: DER SPKI for prime256v1 ends with 0x04 || X || Y; tail 64 bytes = X || Y.
        extractRawCmd = ''
          openssl ec -pubin -in ${pubkeyPem} -pubout -outform DER \
            | tail -c 64 > ${pubkeyRaw}
        '';
        tpmSignHashArg = "-g sha256";
      };
      "ed25519" = {
        createPrimaryArgs = "--key-algorithm ed25519";
        extractRawCmd = ''
          openssl pkey -pubin -in ${pubkeyPem} -outform DER | tail -c 32 > ${pubkeyRaw}
        '';
        tpmSignHashArg = "-g sha256";
      };
    }.${
      cfg.algorithm
    };

  # LOADBEARING: TPMT_SIGNATURE byte layout — ECDSA P-256/SHA-256 raw R‖S at bytes 6..38 (R) + 40..72 (S) of 72-byte struct; ed25519 layout differs.
  extractRawSig = pkgs.writeShellScript "tpm-extract-raw-sig" ''
    set -euo pipefail
    in="$1"
    ${pkgs.coreutils}/bin/dd if="$in" bs=1 skip=6 count=32 status=none
    ${pkgs.coreutils}/bin/dd if="$in" bs=1 skip=40 count=32 status=none
  '';

  signWrapper = pkgs.writeShellApplication {
    name = cfg.signWrapperName;
    runtimeInputs = [pkgs.tpm2-tools pkgs.coreutils];
    text = ''
      set -euo pipefail
      if [ $# -ne 1 ]; then
        echo "usage: ${cfg.signWrapperName} <file>" >&2
        exit 2
      fi

      # tpm2_sign's `-o -` silently produces empty output on this tpm2-tools version; use a tempfile.
      tmpsig="$(mktemp)"
      trap 'rm -f "$tmpsig"' EXIT
      tpm2_sign -c ${cfg.handle} ${algo.tpmSignHashArg} -o "$tmpsig" "$1"
      ${extractRawSig} "$tmpsig"
    '';
  };
in {
  imports = [./options.nix];

  config = lib.mkIf cfg.enable {
    security.tpm2 = {
      enable = true;
      tctiEnvironment.enable = true;
    };

    environment.systemPackages = [
      pkgs.tpm2-tools
      signWrapper
    ];

    nixfleet.keyslots.tpm.signWrapperPackage = signWrapper;

    systemd.services.nixfleet-tpm-keyslot-provision = {
      description = "Provision TPM-backed ${cfg.algorithm} keyslot at ${cfg.handle}";
      wantedBy = ["multi-user.target"];
      after = ["tpm2-abrmd.service" "basic.target"];
      wants = ["tpm2-abrmd.service"];
      serviceConfig = {
        Type = "oneshot";
        RemainAfterExit = true;
        StateDirectory = baseNameOf cfg.exportPubkeyDir;
      };
      path = [pkgs.tpm2-tools pkgs.openssl pkgs.coreutils];
      script = ''
        set -euo pipefail
        mkdir -p ${cfg.exportPubkeyDir}

        extract_raw() {
          ${algo.extractRawCmd}
          chmod 644 ${pubkeyPem} ${pubkeyRaw}
        }

        if tpm2_readpublic -c ${cfg.handle} -f pem -o ${pubkeyPem} 2>/dev/null; then
          extract_raw
          echo "Keyslot already persisted at ${cfg.handle}"
          exit 0
        fi

        tpm2_createprimary \
          --hierarchy o \
          ${algo.createPrimaryArgs} \
          --attributes 'fixedtpm|fixedparent|sensitivedataorigin|userwithauth|sign' \
          --key-context /tmp/nixfleet-tpm-keyslot.ctx
        tpm2_evictcontrol --hierarchy o --object-context /tmp/nixfleet-tpm-keyslot.ctx ${cfg.handle}
        tpm2_readpublic -c ${cfg.handle} -f pem -o ${pubkeyPem}
        extract_raw
        rm -f /tmp/nixfleet-tpm-keyslot.ctx
        echo "${cfg.algorithm} keyslot provisioned at ${cfg.handle}"
      '';
    };

    nixfleet.persistence.directories = [
      {
        directory = cfg.exportPubkeyDir;
        mode = "0755";
      }
    ];
  };
}
