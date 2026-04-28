# TPM-backed signing keyslot. Supports ECDSA P-256 (default, widely
# supported by commodity TPMs) and ed25519 (requires TPM ECC curve 0x0040
# — rare on commercial hardware).
#
# A first-boot one-shot service creates a primary under the owner
# hierarchy with sign-only attributes, evicts it to a persistent handle,
# and exports the public key (PEM + algorithm-specific raw bytes) to a
# configured directory. Idempotent: re-running after an impermanence wipe
# re-extracts the pubkey from the persisted handle without generating a
# new key.
{
  config,
  lib,
  pkgs,
  ...
}: let
  cfg = config.nixfleet.keyslots.tpm;
  pubkeyPem = "${cfg.exportPubkeyDir}/pubkey.pem";
  pubkeyRaw = "${cfg.exportPubkeyDir}/pubkey.raw";

  # Per-algorithm config.
  algo =
    {
      "ecdsa-p256" = {
        # tpm2-tools shorthand for NIST P-256 ECC primary with sha256 hash.
        createPrimaryArgs = "--key-algorithm ecc256:ecdsasha256";
        # DER SubjectPublicKeyInfo for EC prime256v1 ends with a 65-byte
        # uncompressed point (0x04 || X || Y). Extract the last 64 bytes
        # (X || Y) as the raw representation consumers typically pin.
        extractRawCmd = ''
          openssl ec -pubin -in ${pubkeyPem} -pubout -outform DER \
            | tail -c 64 > ${pubkeyRaw}
        '';
        tpmSignHashArg = "-g sha256";
      };
      "ed25519" = {
        createPrimaryArgs = "--key-algorithm ed25519";
        # For ed25519 the raw pubkey is 32 bytes at the tail of the DER SPKI.
        extractRawCmd = ''
          openssl pkey -pubin -in ${pubkeyPem} -outform DER | tail -c 32 > ${pubkeyRaw}
        '';
        tpmSignHashArg = "-g sha256";
      };
    }.${
      cfg.algorithm
    };

  # Parse tpm2_sign's TPMT_SIGNATURE binary output into raw 64-byte
  # R‖S per CONTRACTS.md §II #1. For ECDSA P-256 with SHA-256 the
  # layout is fixed:
  #
  #   offset  bytes    field
  #   0       2        sigAlg            (0x0018 ECDSA)
  #   2       2        hash alg          (0x000B SHA-256)
  #   4       2        signatureR.size   (0x0020 = 32)
  #   6       32       signatureR bytes
  #   38      2        signatureS.size   (0x0020 = 32)
  #   40      32       signatureS bytes
  #
  # Total 72 bytes. We emit bytes 6..38 (R) + bytes 40..72 (S) — the
  # 64-byte raw concatenation the contract requires.
  #
  # For ed25519 the struct shape differs (the full 64-byte sig lives
  # as a single TPM2B_ECC_PARAMETER), but the current deployment uses
  # ecdsa-p256 exclusively; ed25519 extraction can be added when the
  # algorithm is actually exercised.
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

      # tpm2_sign's `-o -` silently produces empty output on this
      # tpm2-tools version (likely interprets `-` as a literal path
      # rather than stdout). Write to a real tempfile, then extract.
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

    # Expose the built wrapper so consumers can reference the derivation
    # directly from other modules (e.g. a CI runner on the same host
    # that needs `tpm-sign` in its unit PATH). Read-only — see options.
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
