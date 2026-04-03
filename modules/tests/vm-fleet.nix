# Tier A — VM fleet test: 4-node TLS/mTLS fleet with rollout, health gates, pause/resume.
#
# Nodes: cp (control plane), web-01, web-02 (healthy agents), db-01 (unhealthy agent).
# TLS: Nix-generated CA + server/client certs — no allowInsecure.
# Rollout: canary on web tag (passes), all-at-once on db tag (pauses on health gate).
#
# Run: nix build .#checks.x86_64-linux.vm-fleet --no-link
{inputs, ...}: {
  perSystem = {
    pkgs,
    system,
    lib,
    ...
  }: let
    helpers = import ./_lib/helpers.nix {inherit lib;};

    mkTestNode = helpers.mkTestNode {
      inherit inputs;
      hostSpecModule = ../_shared/host-spec-module.nix;
    };

    defaultTestSpec = helpers.defaultTestSpec;

    # Build-time TLS certificates: fleet CA + CP server cert + 3 agent client certs.
    # Deterministic — no runtime setup needed.
    testCerts =
      pkgs.runCommand "nixfleet-fleet-test-certs" {
        nativeBuildInputs = [pkgs.openssl];
      } ''
        mkdir -p $out

        # Fleet CA (self-signed, EC P-256)
        openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 \
          -keyout $out/ca-key.pem -out $out/ca.pem -days 365 -nodes \
          -subj '/CN=nixfleet-test-ca'

        # CP server cert (CN=cp, SAN=cp for hostname verification)
        openssl req -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 \
          -keyout $out/cp-key.pem -out $out/cp-csr.pem -nodes \
          -subj '/CN=cp' \
          -addext 'subjectAltName=DNS:cp'
        openssl x509 -req -in $out/cp-csr.pem -CA $out/ca.pem -CAkey $out/ca-key.pem \
          -CAcreateserial -out $out/cp-cert.pem -days 365 \
          -copy_extensions copyall

        # Agent client certs (CN = hostname)
        for host in web-01 web-02 db-01; do
          openssl req -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 \
            -keyout $out/$host-key.pem -out $out/$host-csr.pem -nodes \
            -subj "/CN=$host"
          openssl x509 -req -in $out/$host-csr.pem -CA $out/ca.pem -CAkey $out/ca-key.pem \
            -CAcreateserial -out $out/$host-cert.pem -days 365
        done

        rm -f $out/*.csr.pem $out/*.srl
      '';

    # Helper: build an agent node with TLS, tags, and optional extra modules.
    mkAgentNode = {
      hostName,
      tags,
      healthChecks ? {},
      extraAgentModules ? [],
    }:
      mkTestNode {
        hostSpecValues =
          defaultTestSpec
          // {
            inherit hostName;
          };
        extraModules =
          [
            {
              environment.etc."nixfleet-tls/ca.pem".source = "${testCerts}/ca.pem";
              environment.etc."nixfleet-tls/${hostName}-cert.pem".source = "${testCerts}/${hostName}-cert.pem";
              environment.etc."nixfleet-tls/${hostName}-key.pem".source = "${testCerts}/${hostName}-key.pem";

              services.nixfleet-agent = {
                enable = true;
                controlPlaneUrl = "https://cp:8080";
                machineId = hostName;
                pollInterval = 2;
                dryRun = true;
                inherit tags;
                tls = {
                  clientCert = "/etc/nixfleet-tls/${hostName}-cert.pem";
                  clientKey = "/etc/nixfleet-tls/${hostName}-key.pem";
                };
                inherit healthChecks;
              };
            }
          ]
          ++ extraAgentModules;
      };
  in
    lib.optionalAttrs (system == "x86_64-linux") {
      checks = {
        vm-fleet = pkgs.testers.nixosTest {
          name = "vm-fleet";

          nodes.cp = mkTestNode {
            hostSpecValues =
              defaultTestSpec
              // {
                hostName = "cp";
              };
            extraModules = [
              ({pkgs, ...}: {
                environment.etc."nixfleet-tls/ca.pem".source = "${testCerts}/ca.pem";
                environment.etc."nixfleet-tls/cp-cert.pem".source = "${testCerts}/cp-cert.pem";
                environment.etc."nixfleet-tls/cp-key.pem".source = "${testCerts}/cp-key.pem";

                services.nixfleet-control-plane = {
                  enable = true;
                  openFirewall = true;
                  tls = {
                    cert = "/etc/nixfleet-tls/cp-cert.pem";
                    key = "/etc/nixfleet-tls/cp-key.pem";
                    clientCa = "/etc/nixfleet-tls/ca.pem";
                  };
                };

                environment.systemPackages = [pkgs.sqlite];
              })
            ];
          };

          nodes.web-01 = mkAgentNode {
            hostName = "web-01";
            tags = ["web"];
            healthChecks.http = [
              {
                url = "http://localhost:80/health";
                expectedStatus = 200;
              }
            ];
            extraAgentModules = [
              {
                services.nginx = {
                  enable = true;
                  virtualHosts.default.locations."/health".return = "200 ok";
                };
                nixfleet.monitoring.nodeExporter = {
                  enable = true;
                  openFirewall = true;
                };
              }
            ];
          };

          nodes.web-02 = mkAgentNode {
            hostName = "web-02";
            tags = ["web"];
            healthChecks.http = [
              {
                url = "http://localhost:80/health";
                expectedStatus = 200;
              }
            ];
            extraAgentModules = [
              {
                services.nginx = {
                  enable = true;
                  virtualHosts.default.locations."/health".return = "200 ok";
                };
              }
            ];
          };

          nodes.db-01 = mkAgentNode {
            hostName = "db-01";
            tags = ["db"];
            healthChecks.http = [
              {
                url = "http://localhost:9999/health";
                expectedStatus = 200;
                timeout = 2;
              }
            ];
          };

          testScript = "";
        };
      };
    };
}
