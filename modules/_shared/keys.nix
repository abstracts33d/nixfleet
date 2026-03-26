# SSH public keys used by the framework's test fleet and ISO installer.
# Fleets consuming NixFleet should override sshAuthorizedKeys in their
# org definition (mkOrg { hostSpecDefaults.sshAuthorizedKeys = [...]; }).
# This file exists only so the framework's internal test fleet and ISO
# can build without requiring an external fleet to provide keys.
{
  sshPublicKeys = [
    # NixFleet test key — NOT for production use.
    # Replace with your own key in your fleet's mkOrg definition.
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAINixfleetTestKeyDoNotUseInProduction"
  ];
}
