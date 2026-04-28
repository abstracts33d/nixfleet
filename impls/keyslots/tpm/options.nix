# TPM keyslot scope — option declarations.
{lib, ...}: {
  options.nixfleet.keyslots.tpm = {
    enable = lib.mkEnableOption "TPM2-backed signing keyslot provisioned at first boot";

    handle = lib.mkOption {
      type = lib.types.str;
      default = "0x81010001";
      description = "TPM2 persistent handle for the generated key (TPM2_HT_PERSISTENT range).";
    };

    algorithm = lib.mkOption {
      type = lib.types.enum ["ecdsa-p256" "ed25519"];
      default = "ecdsa-p256";
      description = ''
        Signing algorithm. Commodity TPM2 hardware (Intel PTT, AMD fTPM,
        most discrete TPMs) supports RSA + ECDSA P-256 but not ed25519
        (TPM ECC curve 0x0040 is rare). Use ecdsa-p256 for TPM-backed
        signing; use ed25519 only if the TPM advertises that curve
        (check with `tpm2_getcap ecc-curves`).
      '';
    };

    exportPubkeyDir = lib.mkOption {
      type = lib.types.str;
      default = "/var/lib/nixfleet-tpm-keyslot";
      description = "Directory where the exported public key files are written (PEM + raw).";
    };

    signWrapperName = lib.mkOption {
      type = lib.types.str;
      default = "tpm-sign";
      description = "Name of the shell wrapper installed system-wide that signs a file with the TPM-held key.";
    };

    signWrapperPackage = lib.mkOption {
      type = lib.types.package;
      readOnly = true;
      description = ''
        Read-only: the signing-wrapper derivation the scope builds for
        `signWrapperName`. Exposed so consumers can reference the
        derivation directly (e.g. to extend a CI runner's
        `systemd.services.<name>.path`) rather than going through
        `/run/current-system/sw/bin/<name>` and wrapping with
        `writeShellScriptBin`.
      '';
    };
  };
}
