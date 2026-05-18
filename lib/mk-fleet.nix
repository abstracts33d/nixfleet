# LOADBEARING: output is canonicalized to JCS before signing - no floats, opaque derivations, or attrsets with significant iteration order.
{lib}: let
  inherit (lib) mkOption types;

  # LOADBEARING: selector precedence is `not` > `and` > base OR over (tags, tagsAny, hosts, channel, all); `not`/`and` are recursive.
  selectorType = types.submodule {
    options = {
      tags = mkOption {
        type = types.listOf types.str;
        default = [];
        description = "Host has ALL listed tags.";
      };
      tagsAny = mkOption {
        type = types.listOf types.str;
        default = [];
        description = "Host has ANY listed tag.";
      };
      hosts = mkOption {
        type = types.listOf types.str;
        default = [];
      };
      channel = mkOption {
        type = types.nullOr types.str;
        default = null;
      };
      all = mkOption {
        type = types.bool;
        default = false;
      };
      and = mkOption {
        type = types.listOf selectorType;
        default = [];
        description = "Host matches ALL listed sub-selectors (intersection).";
      };
      not = mkOption {
        type = types.nullOr selectorType;
        default = null;
        description = "Host matches iff it does NOT match the given sub-selector (negation).";
      };
    };
  };

  # Issue #88: declarative commit pin. Same shape on host, tag, channel  -
  # most-specific-wins resolution lives in `resolvePin` below. `expiresAt`
  # is RFC3339 / ISO-8601 string; date arithmetic happens in
  # `nixfleet-release` (chrono) - pure Nix has no robust date parsing.
  pinType = types.submodule {
    options = {
      commit = mkOption {
        type = types.str;
        description = ''
          Source-control rev the host's closure should be built from.
          MUST be a full 40-char SHA - `nixfleet-release` passes this
          verbatim to `nix build "<source>?rev=<commit>#..."`, and Nix's
          flake-ref parser rejects short SHAs / tag names with
          `hash has wrong length for hash algorithm 'sha1'`. Resolve
          short refs operator-side (`git rev-parse <ref>`) before
          declaring the pin.
        '';
      };
      reason = mkOption {
        type = types.str;
        description = ''
          Free-form operator note (CVE ref, audit window, debug
          context). Surfaced verbatim in `nixfleet status` and
          dashboards; not parsed.
        '';
      };
      expiresAt = mkOption {
        type = types.nullOr types.str;
        default = null;
        example = "2026-06-01T00:00:00Z";
        description = ''
          RFC3339 hard expiry. `nixfleet-release` filters expired pins
          at release time so they stop affecting the build path. Hosts
          with an expired pin fall back to the current release commit.
          `null` means no expiry - pin holds until the operator removes
          it from the declaration.
        '';
      };
    };
  };

  hostType = types.submodule {
    options = {
      system = mkOption {type = types.str;};
      configuration = mkOption {
        type = types.nullOr types.unspecified;
        default = null;
        description = ''
          Test-fixture stub-injection slot. Operators leave this at
          the `null` default; the framework builds the per-host
          nixosConfiguration from `nixosArgs` via
          `lib/default.nix`'s `mkFleet` wrapper and exposes the
          result at `fleet.nixosConfigurations`.

          Fixtures under `tests/lib/mk-fleet/` set this to a stub
          attrset carrying `config.system.build.toplevel` so the
          `configurationErrors` invariant has something to inspect
          without spinning up a real NixOS evaluation. See
          `tests/lib/mk-fleet/fixtures/_stub-configuration.nix` for
          the canonical stub. The invariant skips when this is
          `null`, so production fleets never need to populate it.
        '';
      };
      tags = mkOption {
        type = types.listOf types.str;
        default = [];
      };
      channel = mkOption {type = types.str;};
      pubkey = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = ''
          Host SSH ed25519 public key (OpenSSH format). Used by the control
          plane to verify probe-output signatures and bind the host's mTLS
          client cert at enrollment. `null` means the host has not been
          enrolled yet; it appears in the fleet schema but signed artifacts
          from it cannot be verified.
        '';
      };
      pin = mkOption {
        type = types.nullOr pinType;
        default = null;
        description = ''
          Per-host commit pin (issue #88). Most-specific level: when set,
          overrides any tag- or channel-level pin the host is otherwise
          eligible for. See `mkFleet`'s pin precedence: host > tag > channel.
        '';
      };
      nixosArgs = mkOption {
        type = types.attrs;
        default = {};
        description = ''
          Per-host arguments forwarded to `mkHost` by the framework.
          Free-form attrset; `lib/default.nix`'s `mkFleet` wrapper
          merges this with the framework-supplied
          `{ hostName, platform = system, fleetResolved }` and hands
          the result to `mkHost`. Typical keys: `hostSpec`, `modules`,
          `stateVersion`, `isVm`, `extraInputs`.

          Operator pattern (RFC-0011 §2.2 — framework owns the wiring):

            fleet = mkFleet {
              hosts.cp = {
                system = "x86_64-linux";
                channel = "stable";
                nixosArgs.modules = [./hosts/cp];
              };
            };
            nixosConfigurations = fleet.nixosConfigurations;

          The wrapper sets `platform = system` so operators don't repeat
          the platform string. `fleetResolved` is always `fleet.resolved` —
          operators never name it (DEFECT-002 closure-by-framework-design
          per RFC-0011 §3 anti-pattern #4).
        '';
      };
      healthChecks = mkOption {
        type = types.attrsOf healthProbeType;
        default = {};
        description = ''
          Host-scoped probe declarations (RFC-0010 §3.2). Most-specific
          scope: collisions on probe name with tag/fleet scopes resolve
          to the host's value (host > tag > fleet). The agent on this
          host runs the effective probe set; the wave gate consults
          `mode == "enforce"` results.
        '';
      };
    };
  };

  revocationType = types.submodule {
    options = {
      hostname = mkOption {
        type = types.str;
        description = "Hostname whose certs are being revoked.";
      };
      notBefore = mkOption {
        type = types.str;
        description = ''
          RFC3339 timestamp. Any cert for `hostname` whose
          notBefore is strictly older than this is rejected at
          mTLS handshake time.
        '';
      };
      reason = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "Free-form operator note (decommissioned, compromised, rotated, etc.).";
      };
      revokedBy = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "Who declared the revocation. Surfaces in audit logs.";
      };
    };
  };

  bootstrapNonceType = types.submodule {
    options = {
      nonce = mkOption {
        type = types.str;
        description = ''
          Hex-encoded nonce from the token's claims. Matches
          `BootstrapToken.claims.nonce` exactly. CP refuses any
          `/v1/enroll` whose nonce is not present in the signed
          allowlist.
        '';
      };
      hostname = mkOption {
        type = types.str;
        description = ''
          Host this nonce is valid for. Must match the token's
          `claims.hostname`; defends against mis-targeted token swap.
        '';
      };
      expiresAt = mkOption {
        type = types.str;
        description = ''
          RFC3339 timestamp. Authoritative validity window - may be
          tighter than the token's own `expires_at` claim.
          `nixfleet-release` prunes entries with `expiresAt < signedAt`
          before signing the artifact.
        '';
      };
      mintedAt = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "Optional audit trail: when the token was minted.";
      };
      mintedBy = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "Optional audit trail: who minted the token.";
      };
    };
  };

  tagType = types.submodule {
    options = {
      description = mkOption {
        type = types.str;
        default = "";
      };
      pin = mkOption {
        type = types.nullOr pinType;
        default = null;
        description = ''
          Tag-scoped commit pin (issue #88). Applies to every host that
          carries this tag, unless overridden by a host-level pin. A
          host carrying multiple tags that BOTH have pins is rejected
          at eval time - operator must disambiguate (typically by
          moving one pin to a host-level declaration).
        '';
      };
      healthChecks = mkOption {
        type = types.attrsOf healthProbeType;
        default = {};
        description = ''
          Tag-scoped probe declarations (RFC-0010 §3.2). Applies to every
          host carrying this tag. Overridden by a host-level declaration
          with the same probe name; overrides a fleet-level declaration
          with the same probe name (host > tag > fleet precedence).
        '';
      };
    };
  };

  waveType = types.submodule {
    options = {
      selector = mkOption {type = selectorType;};
      soakMinutes = mkOption {
        type = types.int;
        default = 0;
      };
    };
  };

  policyType = types.submodule {
    options = {
      strategy = mkOption {type = types.enum ["canary" "all-at-once" "staged"];};
      waves = mkOption {
        type = types.listOf waveType;
        default = [];
      };
      healthGate = mkOption {
        type = types.attrs;
        default = {};
      };
      onHealthFailure = mkOption {
        type = types.enum ["halt" "rollback-and-halt"];
        default = "rollback-and-halt";
      };
    };
  };

  # RFC-0010 §3.5 channel-scope compliance shorthand. Each entry is
  # either a bare framework name (inherits compliance.mode) or
  # `{ name; mode ? null; }` for per-framework override. Desugars to
  # `healthChecks.evidence-<name>` entries via
  # `channelEffectiveHealthChecks` below.
  complianceFrameworkEntryType = types.submodule {
    options = {
      name = mkOption {type = types.str;};
      mode = mkOption {
        type = types.nullOr (types.enum ["enforce" "observe"]);
        default = null;
        description = ''
          Per-framework mode. `null` inherits `compliance.mode`.
        '';
      };
      controlOverrides = mkOption {
        type = types.attrsOf (types.submodule {
          options = {
            mode = mkOption {
              type = types.enum ["enforce" "observe" "disabled"];
            };
            reason = mkOption {
              type = types.str;
              default = "";
            };
          };
        });
        default = {};
        description = ''
          Per-control mode overrides scoped to this framework. Keys
          are nixfleet-compliance control IDs (e.g.
          `"access-control"`). Desugars to the same field on the
          synthesised `evidence-<framework>` probe; the agent's
          effective-mode resolution uses these overrides on top of
          the per-framework `mode`.

          Example:

            compliance.frameworks = [
              {
                name = "nis2-essential";
                mode = "enforce";
                controlOverrides = {
                  "agent-egress-exemption" = {
                    mode = "observe";
                    reason = "Phase-out window for legacy egress policy";
                  };
                };
              }
            ];
        '';
      };
    };
  };

  complianceType = types.submodule {
    options = {
      frameworks = mkOption {
        type = types.listOf (types.either types.str complianceFrameworkEntryType);
        default = [];
      };
      mode = mkOption {
        type = types.enum ["enforce" "observe"];
        default = "enforce";
        description = ''
          Default mode for bare-string entries in `frameworks`.
          Per-entry attrsets with `mode = "..."` override this.
        '';
      };
    };
  };

  channelType = types.submodule {
    options = {
      description = mkOption {
        type = types.str;
        default = "";
      };
      pin = mkOption {
        type = types.nullOr pinType;
        default = null;
        description = ''
          Channel-scoped commit pin (issue #88). Applies to every host on
          this channel that does NOT carry a more-specific (host- or tag-
          level) pin. Useful for "freeze stable on commit X during the
          audit window" semantics.
        '';
      };
      rolloutPolicy = mkOption {type = types.str;};
      reconcileIntervalMinutes = mkOption {
        type = types.int;
        default = 30;
      };
      signingIntervalMinutes = mkOption {
        type = types.int;
        default = 60;
        description = ''
          How often CI re-signs `fleet.resolved` for this channel.
          Sets the replay-defense floor: a consumer accepts an artifact for
          at least this long before refresh is expected.
        '';
      };
      freshnessWindow = mkOption {
        type = types.int;
        description = ''
          Minutes a signed `fleet.resolved` artifact is accepted by agents
          after `meta.signedAt`. MUST be ≥ 2 × signingIntervalMinutes so a
          single missed signing run does not strand agents.
        '';
      };
      healthChecks = mkOption {
        type = types.attrsOf healthProbeType;
        default = {};
        description = ''
          Channel-scoped probe declarations (RFC-0010 §3.5). Applies to
          every host whose `channel` field equals this channel's name.
          Sits between tag and host in the resolution order: host scope
          overrides channel; channel overrides tag; tag overrides fleet
          (host > channel > tag > fleet precedence). Same merge + shadow-
          warning semantics as the other scopes.
        '';
      };
      compliance = mkOption {
        type = complianceType;
        default = {};
        description = ''
          Channel-wide compliance-framework shorthand (RFC-0010 §3.5).
          Desugars to one `evidence-<framework>` entry in
          `healthChecks` per framework. Pure Nix expansion - no
          proto/runtime impact; the wire layer only ever sees
          `healthChecks`.

          Two list shapes accepted, freely mixed:

            compliance.frameworks = [ "anssi-bp028" "nis2" ];
            # bare names; both inherit compliance.mode (default
            # "enforce")

            compliance.frameworks = [
              { name = "anssi-bp028"; mode = "enforce"; }
              { name = "nis2"; mode = "observe"; }
            ];
            # per-framework mode

          Synthesized probe name is `evidence-<framework>` -
          LOADBEARING for operator dashboards built against
          `probe_failures` / Prometheus labels. Collisions with an
          explicit `healthChecks.evidence-<framework>` entry throw at
          eval time.

          For custom evidence paths or non-default intervals, declare
          the probe explicitly under `healthChecks` instead of using
          this shorthand.
        '';
      };
    };
  };

  # Per-probe declaration (RFC-0010 §3.1). The `kind` enum:
  #   http     — GET <url>; Pass iff response status == expectStatus.
  #   tcp      — connect host:port; Pass iff connect succeeds within
  #              connectTimeoutSecs.
  #   exec     — exec command (argv); Pass iff exit 0 within timeoutSecs.
  #   evidence — read /var/lib/nixfleet-compliance/evidence.json + verify
  #              ed25519 signature against host SSH host pubkey
  #              (RFC-0004 §5); Pass iff signature valid AND framework's
  #              controls all pass. Decoupled from collector cadence.
  #
  # `mode` (RFC-0010 §3.4) is per-probe:
  #   enforce  — wave gate consults latest result; Fail blocks promotion.
  #   observe  — result recorded in event_log; gate ignores it.
  #   disabled — declared but agent does not run it (operator suppression).
  #
  # `intervalSeconds` vs `runOnce`: mutually exclusive. `runOnce = true`
  # means "run on activation, never again" (one-shot evidence collection).
  # `intervalSeconds = 0` is rejected at fleet-eval time (ambiguous).
  healthProbeType = types.submodule {
    options = {
      kind = mkOption {
        type = types.enum ["http" "tcp" "exec" "evidence"];
      };
      mode = mkOption {
        type = types.enum ["enforce" "observe" "disabled"];
        default = "enforce";
      };
      intervalSeconds = mkOption {
        type = types.int;
        default = 30;
        description = ''
          Run cadence in seconds. Lower bound 5s. `0` is rejected at
          fleet-eval time (ambiguous); use `runOnce = true` for
          one-shot semantics.
        '';
      };
      runOnce = mkOption {
        type = types.bool;
        default = false;
        description = ''
          Run once on activation, never again. Mutually exclusive with
          `intervalSeconds` (eval rejects when both set non-default).
        '';
      };
      # http
      url = mkOption {
        type = types.nullOr types.str;
        default = null;
      };
      expectStatus = mkOption {
        type = types.int;
        default = 200;
      };
      # tcp
      host = mkOption {
        type = types.nullOr types.str;
        default = null;
      };
      port = mkOption {
        type = types.nullOr types.int;
        default = null;
      };
      connectTimeoutSecs = mkOption {
        type = types.int;
        default = 5;
      };
      # exec
      command = mkOption {
        type = types.listOf types.str;
        default = [];
      };
      timeoutSecs = mkOption {
        type = types.int;
        default = 10;
      };
      # evidence
      framework = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = ''
          Framework name (e.g. `"nis2-essential"`). The agent
          evaluates every control covering this framework per the
          collector's evidence.json. Mutually exclusive with
          `controls` — declare one or the other.
        '';
      };
      evidencePath = mkOption {
        type = types.str;
        default = "/var/lib/nixfleet-compliance/evidence.json";
      };
      # Per-control mode overrides on top of probe-level `mode`. Keys
      # are nixfleet-compliance control IDs (capability-named, e.g.
      # `"access-control"`, `"secure-boot"`). Values are a richer
      # attrset:
      #
      #   controlOverrides = {
      #     "access-control" = { mode = "observe"; reason = "..."; };
      #   };
      #
      # The agent's evidence runner resolves effective_mode per
      # control: override > probe-level mode. A control with
      # effective_mode = "observe" contributes to sub_results but
      # does not gate the wave; "disabled" controls are dropped from
      # sub_results entirely. The `reason` is operator-facing audit
      # rationale (surfaced in event_log + dashboards).
      controlOverrides = mkOption {
        type = types.attrsOf (types.submodule {
          options = {
            mode = mkOption {
              type = types.enum ["enforce" "observe" "disabled"];
              description = ''
                Effective mode for this control. Overrides the
                probe-level `mode`. See controlOverrides docstring
                on the parent for semantics.
              '';
            };
            reason = mkOption {
              type = types.str;
              default = "";
              description = ''
                Operator-facing audit rationale. Surfaced in event_log
                + compliance dashboards. Recommended; empty string is
                accepted but discouraged for `disabled`/`observe`
                overrides since auditors expect a documented
                exemption rationale.
              '';
            };
          };
        });
        default = {};
        description = ''
          Per-control mode overrides for evidence probes. Keys are
          control IDs (e.g. `"access-control"`), values are
          `{ mode; reason; }` attrsets. See the inline comment for
          full semantics; the agent's per-control mode resolution
          uses these.
        '';
      };
      # Explicit per-control selection. Operators compose a custom
      # framework by listing the controls + per-control modes. The
      # native framework field on each control (from the collector's
      # frameworkArticles map) stays on the wire for auditor
      # visibility; this declaration just picks which controls to
      # gate on. Mutually exclusive with `framework`.
      #
      #   controls = {
      #     "access-control"      = { mode = "enforce"; };
      #     "secure-boot"         = { mode = "enforce"; reason = "..."; };
      #     "agent-egress-exemption" = { mode = "observe"; };
      #   };
      controls = mkOption {
        type = types.attrsOf (types.submodule {
          options = {
            mode = mkOption {
              type = types.enum ["enforce" "observe" "disabled"];
              default = "enforce";
            };
            reason = mkOption {
              type = types.str;
              default = "";
            };
          };
        });
        default = {};
        description = ''
          Explicit control list with per-control modes (custom-
          framework declaration). Each key is a control ID; each
          value is `{ mode; reason; }`. The agent gates on this set
          regardless of the controls' native framework affiliations.
          Mutually exclusive with `framework`; eval rejects probes
          that set both.
        '';
      };
    };
  };

  # Host-level DAG edge. `gated` waits for `gates` to reach Soaked /
  # Converged within the same rollout. Both hosts MUST be on the same
  # channel - cross-channel ordering is `channelEdges`'s job.
  hostEdgeType = types.submodule {
    options = {
      gated = mkOption {
        type = types.str;
        description = "Host whose dispatch is held until `gates` completes.";
      };
      gates = mkOption {
        type = types.str;
        description = "Host that must reach Soaked/Converged before `gated` dispatches.";
      };
      reason = mkOption {
        type = types.str;
        default = "";
      };
    };
  };

  # Cross-channel ordering edge. Canonical names match the host-level
  # `Edge` (gated/gates): `gates` is the predecessor that runs first;
  # `gated` is the dependent that holds until `gates` converges.
  #
  # `before`/`after` are accepted as deprecated aliases - older
  # fleet.nix files keep working without an atomic rename. The
  # validation block below errors if both legacy + canonical names
  # are set on the same edge so the operator picks one shape.
  channelEdgeType = types.submodule {
    options = {
      gates = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "Predecessor channel - must converge before `gated` opens.";
      };
      gated = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "Dependent channel - held until `gates` converges.";
      };
      before = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "DEPRECATED alias for `gates`. Update to `gates` on next edit.";
      };
      after = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "DEPRECATED alias for `gated`. Update to `gated` on next edit.";
      };
      reason = mkOption {
        type = types.str;
        default = "";
      };
    };
  };

  # Normalize a channelEdge record from either field naming to
  # `{gates, gated, reason}`. Errors when both shapes are set on
  # the same edge - the operator must pick one.
  normalizeChannelEdge = e: let
    g =
      if e.gates != null
      then e.gates
      else e.before;
    d =
      if e.gated != null
      then e.gated
      else e.after;
  in {
    gates = g;
    gated = d;
    reason = e.reason;
  };

  budgetType = types.submodule {
    options = {
      selector = mkOption {type = selectorType;};
      maxInFlight = mkOption {
        type = types.nullOr types.int;
        default = null;
      };
      maxInFlightPct = mkOption {
        type = types.nullOr types.int;
        default = null;
      };
    };
  };

  # LOADBEARING: edges are walked "gated -> gates" (dependent -> predecessor).
  # Operates on the NORMALIZED form (post `normalizeChannelEdge`) so legacy
  # `before`/`after` and canonical `gates`/`gated` shapes both flow through
  # the same DAG check.
  hasCycle = edges: let
    adj =
      lib.foldl' (
        acc: e: let
          current = acc.${e.gated} or [];
        in
          acc // {${e.gated} = current ++ [e.gates];}
      ) {}
      edges;
    nodes = lib.unique (map (e: e.gated) edges ++ map (e: e.gates) edges);
    visit = node: path: visited:
      if builtins.elem node path
      then {
        cycle = true;
        path = path ++ [node];
        visited = visited;
      }
      else if builtins.elem node visited
      then {
        cycle = false;
        path = path;
        visited = visited;
      }
      else let
        children = adj.${node} or [];
        walk = c: acc:
          if acc.cycle
          then acc
          else let
            r = visit c (path ++ [node]) acc.visited;
          in
            if r.cycle
            then r
            else {
              cycle = false;
              path = acc.path;
              visited = r.visited ++ [c];
            };
        result =
          lib.foldl' (a: c: walk c a) {
            cycle = false;
            path = [];
            visited = visited;
          }
          children;
      in
        if result.cycle
        then result
        else {
          cycle = false;
          path = [];
          visited = result.visited ++ [node];
        };
    scan = nodes:
      lib.foldl' (
        acc: n:
          if acc.cycle
          then acc
          else visit n [] acc.visited
      ) {
        cycle = false;
        path = [];
        visited = [];
      }
      nodes;
  in
    (scan nodes).cycle;

  # RFC-0010 §3.5: compliance shorthand desugars to evidence-<framework>
  # probes merged into the channel's explicit healthChecks. Two callers
  # (checkInvariants' probeDeclSites and resolveFleet's
  # resolveHealthChecks + healthCheckOverrideWarnings) consume the
  # effective set, so the helper lives at file scope and takes the
  # already-evaluated cfg.
  #
  # Synthesized probes carry all healthProbeType defaults explicitly
  # because they bypass submodule evaluation (cfg.channels.<name> is
  # already evaluated by the time this runs). If healthProbeType's
  # defaults change, this must stay in sync.
  normalizeComplianceEntry = defaultMode: entry:
    if builtins.isString entry
    then {
      name = entry;
      mode = defaultMode;
      controlOverrides = {};
    }
    else {
      inherit (entry) name;
      mode =
        if entry.mode == null
        then defaultMode
        else entry.mode;
      controlOverrides = entry.controlOverrides or {};
    };

  channelEffectiveHealthChecks = cfg: channelName: let
    c = cfg.channels.${channelName};
    synthesized = lib.listToAttrs (map (raw: let
        fw = normalizeComplianceEntry c.compliance.mode raw;
      in {
        name = "evidence-${fw.name}";
        value = {
          kind = "evidence";
          mode = fw.mode;
          framework = fw.name;
          inherit (fw) controlOverrides;
          controls = {};
          evidencePath = "/var/lib/nixfleet-compliance/evidence.json";
          intervalSeconds = 30;
          runOnce = false;
          url = null;
          expectStatus = 200;
          host = null;
          port = null;
          connectTimeoutSecs = 5;
          command = [];
          timeoutSecs = 10;
        };
      })
      c.compliance.frameworks);
    collisions =
      builtins.filter
      (k: builtins.elem k (lib.attrNames c.healthChecks))
      (lib.attrNames synthesized);
  in
    if collisions != []
    then throw "channel '${channelName}': compliance.frameworks synthesizes probe(s) [${lib.concatStringsSep ", " collisions}] that collide with explicit healthChecks entries. Remove the framework from compliance.frameworks, or rename the explicit healthChecks entry."
    else c.healthChecks // synthesized;

  resolveSelector = sel: hosts: let
    names = lib.attrNames hosts;
    matchHost = s: n: h:
      if s.not != null
      then !(matchHost s.not n h)
      else if s.and != []
      then lib.all (sub: matchHost sub n h) s.and
      else
        s.all
        || (s.hosts != [] && builtins.elem n s.hosts)
        || (s.channel != null && h.channel == s.channel)
        || (s.tags != [] && lib.all (t: builtins.elem t h.tags) s.tags)
        || (s.tagsAny != [] && lib.any (t: builtins.elem t h.tags) s.tagsAny);
  in
    builtins.filter (n: matchHost sel n hosts.${n}) names;

  checkInvariants = cfg: let
    hostNames = lib.attrNames cfg.hosts;
    channelNames = lib.attrNames cfg.channels;
    policyNames = lib.attrNames cfg.rolloutPolicies;

    hostChannelErrors =
      lib.concatMap (
        n:
          lib.optional (!builtins.elem cfg.hosts.${n}.channel channelNames)
          "host '${n}' references unknown channel '${cfg.hosts.${n}.channel}'"
      )
      hostNames;

    channelPolicyErrors =
      lib.concatMap (
        n:
          lib.optional (!builtins.elem cfg.channels.${n}.rolloutPolicy policyNames)
          "channel '${n}' references unknown rollout policy '${cfg.channels.${n}.rolloutPolicy}'"
      )
      channelNames;

    edgeErrors =
      lib.concatMap (
        e:
          lib.optional (!builtins.elem e.gated hostNames) "edge.gated references unknown host '${e.gated}'"
          ++ lib.optional (!builtins.elem e.gates hostNames) "edge.gates references unknown host '${e.gates}'"
          ++ lib.optional (
            (cfg.hosts.${e.gated}.channel or null)
            != null
            && (cfg.hosts.${e.gates}.channel or null) != null
            && cfg.hosts.${e.gated}.channel != cfg.hosts.${e.gates}.channel
          ) "edge.gated '${e.gated}' (channel '${cfg.hosts.${e.gated}.channel}') and edge.gates '${e.gates}' (channel '${cfg.hosts.${e.gates}.channel}') are on different channels; use channelEdges for cross-channel ordering"
      )
      cfg.edges;

    # Detect mixed-shape entries before normalization - operator must
    # pick one naming. Cleaner than silently picking gates+after.
    channelEdgeShapeErrors =
      lib.concatMap (
        e:
          lib.optional ((e.gates != null) && (e.before != null))
          "channelEdges entry sets both `gates` and `before` - pick one (canonical: `gates`)"
          ++ lib.optional ((e.gated != null) && (e.after != null))
          "channelEdges entry sets both `gated` and `after` - pick one (canonical: `gated`)"
          ++ lib.optional ((e.gates == null) && (e.before == null))
          "channelEdges entry must set `gates` (or legacy alias `before`)"
          ++ lib.optional ((e.gated == null) && (e.after == null))
          "channelEdges entry must set `gated` (or legacy alias `after`)"
      )
      cfg.channelEdges;

    # All downstream channelEdge-aware code reads from this normalized
    # list. Single shape ⇒ no per-site if-else for the legacy fields.
    normalizedChannelEdges = map normalizeChannelEdge cfg.channelEdges;

    channelEdgeErrors =
      lib.concatMap (
        e:
          lib.optional (!builtins.elem e.gates channelNames) "channelEdges.gates references unknown channel '${e.gates}'"
          ++ lib.optional (!builtins.elem e.gated channelNames) "channelEdges.gated references unknown channel '${e.gated}'"
          ++ lib.optional (e.gates == e.gated) "channelEdges entry has gates == gated ('${e.gates}'); use a wave-staged policy for intra-channel ordering instead"
      )
      normalizedChannelEdges;

    # Two operator paths converge here:
    #   - operator pre-builds the nixosConfiguration and passes it as
    #     `configuration` (validated below before mkFleet consumes it)
    #   - operator declares `nixosArgs`, the framework `mkFleet`
    #     wrapper builds via `mkHost` itself, and `configuration`
    #     stays at its `null` default — this branch skips when null
    #     (mkHost's own evaluation catches structural errors
    #     downstream).
    configurationErrors =
      lib.concatMap (
        n: let
          h = cfg.hosts.${n};
          isValid =
            h.configuration
            == null
            || (
              builtins.isAttrs h.configuration
              && h.configuration ? config
              && h.configuration.config ? system
              && h.configuration.config.system ? build
              && h.configuration.config.system.build ? toplevel
            );
        in
          lib.optional (!isValid)
          "host '${n}' configuration is not a valid nixosConfiguration (missing config.system.build.toplevel) and `nixosArgs` is not set"
      )
      hostNames;

    # RFC-0010 §10: validate probe declarations across all four scopes.
    # `intervalSeconds = 0` is ambiguous; the runner uses `runOnce = true`
    # for one-shot semantics. `runOnce + intervalSeconds` set together is
    # also ambiguous.
    probeDeclSites =
      lib.mapAttrsToList (n: p: {
        scope = "fleet:${n}";
        probe = p;
      })
      cfg.healthChecks
      ++ lib.concatMap (
        tag:
          lib.mapAttrsToList (n: p: {
            scope = "tag:${tag}:${n}";
            probe = p;
          }) (cfg.tags.${tag}.healthChecks or {})
      )
      (lib.attrNames cfg.tags)
      ++ lib.concatMap (
        channel:
          lib.mapAttrsToList (n: p: {
            scope = "channel:${channel}:${n}";
            probe = p;
          }) (channelEffectiveHealthChecks cfg channel)
      )
      (lib.attrNames cfg.channels)
      ++ lib.concatMap (
        host:
          lib.mapAttrsToList (n: p: {
            scope = "host:${host}:${n}";
            probe = p;
          })
          cfg.hosts.${host}.healthChecks
      )
      (lib.attrNames cfg.hosts);

    probeDeclErrors =
      lib.concatMap (
        site: let
          p = site.probe;
          intervalZero = p.intervalSeconds == 0 && !p.runOnce;
          runOnceConflict = p.runOnce && p.intervalSeconds != 30; # 30 is the option default
          httpMissingUrl = p.kind == "http" && p.url == null;
          tcpMissingPort = p.kind == "tcp" && p.port == null;
          execMissingCommand = p.kind == "exec" && p.command == [];
          # Evidence probes select controls one of two ways:
          #   (a) `framework = "<name>"` — agent evaluates every
          #       control whose frameworkArticles map contains the
          #       framework. Optional `controlOverrides` map applies
          #       per-control mode tweaks on top of `mode`.
          #   (b) `controls = { ... }` — explicit custom-framework
          #       declaration, one entry per control with mode.
          # Exactly one must be set.
          hasFramework = p.framework != null;
          hasControls = (p.controls or {}) != {};
          evidenceNeitherSet = p.kind == "evidence" && !hasFramework && !hasControls;
          evidenceBothSet = p.kind == "evidence" && hasFramework && hasControls;
          overridesWithoutFramework =
            p.kind == "evidence" && !hasFramework && (p.controlOverrides or {}) != {};
        in
          lib.optional intervalZero "probe ${site.scope}: intervalSeconds = 0 is ambiguous; use runOnce = true for one-shot semantics"
          ++ lib.optional runOnceConflict "probe ${site.scope}: runOnce = true conflicts with non-default intervalSeconds"
          ++ lib.optional httpMissingUrl "probe ${site.scope}: kind=http requires url"
          ++ lib.optional tcpMissingPort "probe ${site.scope}: kind=tcp requires port"
          ++ lib.optional execMissingCommand "probe ${site.scope}: kind=exec requires non-empty command"
          ++ lib.optional evidenceNeitherSet "probe ${site.scope}: kind=evidence requires either framework or controls"
          ++ lib.optional evidenceBothSet "probe ${site.scope}: kind=evidence accepts framework XOR controls, not both"
          ++ lib.optional overridesWithoutFramework "probe ${site.scope}: controlOverrides only applies with framework set; use controls for explicit per-control declaration"
      )
      probeDeclSites;

    # `hasCycle` expects edges with `gates`/`gated`. Host-level `edges`
    # already use that schema; channelEdges flow through
    # `normalizedChannelEdges` so legacy `before`/`after` entries also
    # work without per-site translation.
    cycleErrors = lib.optional (hasCycle cfg.edges) "edges form a cycle; the DAG invariant is violated";

    channelCycleErrors = lib.optional (hasCycle normalizedChannelEdges) "channelEdges form a cycle; cross-channel ordering must be a DAG";

    freshnessErrors =
      lib.concatMap (
        channelName: let
          c = cfg.channels.${channelName};
        in
          lib.optional (c.freshnessWindow < 2 * c.signingIntervalMinutes)
          "channel '${channelName}': freshnessWindow (${toString c.freshnessWindow}) must be ≥ 2 × signingIntervalMinutes (${toString c.signingIntervalMinutes})"
      )
      (lib.attrNames cfg.channels);

    # Issue #88: ambiguity error when a host carries 2+ tags whose pins
    # both resolve. Eager: lives in checkInvariants so the harness's
    # tryEval (which doesn't force lazy mapAttrs thunks) catches it.
    pinTagConflictErrors =
      lib.concatMap (
        hostName: let
          host = cfg.hosts.${hostName};
          pinnedTagNames =
            lib.filter (
              t: (cfg.tags ? ${t}) && (cfg.tags.${t}.pin or null) != null
            )
            host.tags;
        in
          # Only conflicts when host doesn't have its own pin (host wins).
          lib.optional
          (host.pin == null && builtins.length pinnedTagNames > 1)
          "host '${hostName}' is in multiple tags with pins (${
            lib.concatStringsSep ", " pinnedTagNames
          }) - disambiguate by lifting one of these pins to a host-level declaration on '${hostName}', or removing the pin from one of the tags"
      )
      (lib.attrNames cfg.hosts);

    errs = hostChannelErrors ++ channelPolicyErrors ++ edgeErrors ++ channelEdgeShapeErrors ++ channelEdgeErrors ++ configurationErrors ++ probeDeclErrors ++ cycleErrors ++ channelCycleErrors ++ freshnessErrors ++ pinTagConflictErrors;
  in
    if errs == []
    then true
    else throw ("nixfleet invariant violations:\n  - " + lib.concatStringsSep "\n  - " errs);

  resolveFleet = cfg:
    assert checkInvariants cfg; let
      emptySelectorWarnings =
        lib.concatMap (
          policyName:
            lib.concatMap (
              w: let
                hosts = resolveSelector w.selector cfg.hosts;
              in
                lib.optional (hosts == [])
                "rollout policy '${policyName}' has a wave with a selector that resolves to zero hosts"
            )
            cfg.rolloutPolicies.${policyName}.waves
        )
        (lib.attrNames cfg.rolloutPolicies);

      budgetWarnings =
        lib.concatMap (
          b: let
            hosts = resolveSelector b.selector cfg.hosts;
            effectiveMax =
              if b.maxInFlight != null
              then b.maxInFlight
              else if b.maxInFlightPct != null
              then lib.max 1 ((builtins.length hosts * b.maxInFlightPct) / 100)
              else builtins.length hosts;
          in
            lib.optional (builtins.length hosts >= 10 && effectiveMax == 1)
            "disruption budget with maxInFlight=1 on ${toString (builtins.length hosts)} hosts will take long to complete"
        )
        cfg.disruptionBudgets;

      # RFC-0010 §3.5 + §4 — resolve effective probe set per host with
      # host > channel > tag > fleet precedence + collision warnings.
      # The merged set flows into each host's closure via
      # `effectiveHealthChecks` in the resolved output; the host module
      # renders it as `/etc/nixfleet/agent/health-checks.json`.
      resolveHealthChecks = hostName: let
        host = cfg.hosts.${hostName};
        fleetScope = cfg.healthChecks;
        tagScopes =
          lib.foldl' (
            acc: tag:
              if cfg.tags ? ${tag}
              then acc // (cfg.tags.${tag}.healthChecks or {})
              else acc
          )
          {}
          host.tags;
        channelScope =
          if cfg.channels ? ${host.channel}
          then channelEffectiveHealthChecks cfg host.channel
          else {};
        # Precedence (lowest -> highest, later wins):
        merged = fleetScope // tagScopes // channelScope // host.healthChecks;
      in
        merged;

      healthCheckOverrideWarnings = lib.concatLists (
        lib.mapAttrsToList (
          hostName: host: let
            fleetNames = lib.attrNames cfg.healthChecks;
            tagNames =
              lib.concatMap (
                tag:
                  if cfg.tags ? ${tag}
                  then lib.attrNames (cfg.tags.${tag}.healthChecks or {})
                  else []
              )
              host.tags;
            channelNames =
              if cfg.channels ? ${host.channel}
              then lib.attrNames (channelEffectiveHealthChecks cfg host.channel)
              else [];
            hostNames = lib.attrNames host.healthChecks;
            hostShadowsChannel =
              lib.filter (n: builtins.elem n channelNames) hostNames;
            hostShadowsTag =
              lib.filter (n: builtins.elem n tagNames) hostNames;
            hostShadowsFleet =
              lib.filter (n: builtins.elem n fleetNames) hostNames;
            channelShadowsTag =
              lib.filter (n: builtins.elem n tagNames) channelNames;
            channelShadowsFleet =
              lib.filter (n: builtins.elem n fleetNames) channelNames;
            tagShadowsFleet =
              lib.filter (n: builtins.elem n fleetNames) tagNames;
            messages =
              map (n: "[healthChecks] host '${hostName}' overrides channel-scoped probe '${n}' (channel '${host.channel}')") hostShadowsChannel
              ++ map (n: "[healthChecks] host '${hostName}' overrides tag-scoped probe '${n}'") hostShadowsTag
              ++ map (n: "[healthChecks] host '${hostName}' overrides fleet-scoped probe '${n}'") hostShadowsFleet
              ++ map (n: "[healthChecks] channel-scoped probe '${n}' (channel '${host.channel}') on host '${hostName}' overrides tag-scoped probe of same name") channelShadowsTag
              ++ map (n: "[healthChecks] channel-scoped probe '${n}' (channel '${host.channel}') on host '${hostName}' overrides fleet-scoped probe of same name") channelShadowsFleet
              ++ map (n: "[healthChecks] tag-scoped probe '${n}' on host '${hostName}' overrides fleet-scoped probe of same name") tagShadowsFleet;
          in
            messages
        )
        cfg.hosts
      );

      # Issue #88: most-specific-wins pin resolution (host > tag > channel).
      # Multi-tag-conflict is rejected eagerly in `checkInvariants`, so by
      # the time we get here at most one tag pin can apply. `expiresAt`
      # filtering happens later in `nixfleet-release` (chrono-based RFC3339
      # comparison); pure Nix has no robust date parsing.
      resolvePin = hostName: let
        host = cfg.hosts.${hostName};
        pinnedTagNames =
          lib.filter (
            t: (cfg.tags ? ${t}) && (cfg.tags.${t}.pin or null) != null
          )
          host.tags;
        tagPin =
          if pinnedTagNames == []
          then null
          else cfg.tags.${builtins.head pinnedTagNames}.pin;
        channelPin = cfg.channels.${host.channel}.pin or null;
      in
        if host.pin != null
        then host.pin
        else if tagPin != null
        then tagPin
        else channelPin;

      allWarnings =
        emptySelectorWarnings
        ++ budgetWarnings
        ++ healthCheckOverrideWarnings;

      # LOADBEARING: builtins.seq below forces the warning side-effect before return.
      emittedWarnings =
        lib.foldl' (acc: msg: lib.warn msg acc) null allWarnings;

      resolved = {
        schemaVersion = 1;
        meta = {
          schemaVersion = 1;
          signedAt = null;
          ciCommit = null;
          # signatureAlgorithm intentionally absent here - pre-stamp eval
          # has no signature, so claiming an algorithm would be a lie.
          # docs/design/contracts.md §V Pattern A: absent ≡ "ed25519" within
          # schemaVersion 1. `stamp_meta` populates the real algorithm at
          # CI signing time.
        };
        hosts =
          lib.mapAttrs (
            n: h: let
              pin = resolvePin n;
            in
              {
                inherit (h) system tags channel pubkey;
                closureHash = null; # Filled by CI from h.configuration.config.system.build.toplevel.
              }
              // lib.optionalAttrs (pin != null) {inherit pin;}
          )
          cfg.hosts;
        channels =
          lib.mapAttrs (_: c: {
            inherit (c) rolloutPolicy reconcileIntervalMinutes signingIntervalMinutes freshnessWindow;
          })
          cfg.channels;
        # Per-host effective probe set, host > tag > fleet precedence
        # (RFC-0010 §3.2). NOT part of the signed manifest schema — the
        # closure hash chain transitively signs the topology via the
        # host's NixOS module rendering this into
        # `/etc/nixfleet/agent/health-checks.json`. The CP never reads
        # this map; it's exposed here so `nixfleet-release` /
        # `mkHost` can plumb the resolved set into each host's closure.
        effectiveHealthChecks =
          lib.mapAttrs (n: _: resolveHealthChecks n) cfg.hosts;
        rolloutPolicies = cfg.rolloutPolicies;
        waves =
          lib.mapAttrs (
            _: c:
              map (w: {
                hosts = resolveSelector w.selector cfg.hosts;
                soakMinutes = w.soakMinutes;
              })
              cfg.rolloutPolicies.${c.rolloutPolicy}.waves
          )
          cfg.channels;
        edges = cfg.edges;
        # Emit canonical {gates, gated, reason} regardless of whether the
        # operator wrote `before/after` or `gates/gated` in fleet.nix.
        # Old fleet.resolved.json bytes (legacy `before/after`) still
        # verify on upgraded CPs via the proto's serde alias.
        channelEdges = map normalizeChannelEdge cfg.channelEdges;
        # Selector preserved at wire level (was: expanded to hosts at eval).
        # Reconciler resolves dynamically so adding/removing a tagged host
        # doesn't require re-signing fleet.resolved. Pre-feat-channel-edges
        # consumers that read `hosts:[]` will see this field absent and must
        # be upgraded - the reconciler in this PR handles either shape.
        disruptionBudgets =
          map (b: {
            selector = b.selector;
            maxInFlight = b.maxInFlight;
            maxInFlightPct = b.maxInFlightPct;
          })
          cfg.disruptionBudgets;
      };
    in
      builtins.seq emittedWarnings resolved;

  # GOTCHA: signatureAlgorithm omitted defaults to ed25519 for backward compat.
  withSignature = {
    signedAt,
    ciCommit,
    signatureAlgorithm ? null,
  }: resolved:
    resolved
    // {
      meta =
        resolved.meta
        // {inherit signedAt ciCommit;}
        // lib.optionalAttrs (signatureAlgorithm != null) {inherit signatureAlgorithm;};
    };
  mkFleet = input: let
    evaluated = lib.evalModules {
      modules = [
        {
          options = {
            hosts = mkOption {
              type = types.attrsOf hostType;
              default = {};
            };
            tags = mkOption {
              type = types.attrsOf tagType;
              default = {};
            };
            channels = mkOption {
              type = types.attrsOf channelType;
              default = {};
            };
            rolloutPolicies = mkOption {
              type = types.attrsOf policyType;
              default = {};
            };
            edges = mkOption {
              type = types.listOf hostEdgeType;
              default = [];
              description = ''
                Per-host DAG ordering within a rollout. `gated` host's
                dispatch is held until `gates` host reaches
                Soaked/Converged within the same rollout. Both hosts
                must be on the same channel - cross-channel ordering
                is `channelEdges`'s job.
              '';
            };
            channelEdges = mkOption {
              type = types.listOf channelEdgeType;
              default = [];
              description = ''
                Cross-channel rollout ordering. A new rollout on channel
                `after` is held until the most-recent rollout on channel
                `before` reaches Converged. Validated at eval time:
                channels must exist and edges must form a DAG.

                Within-channel coordination uses `edges` (host-level);
                this is RFC-0002 §4.3's cross-channel primitive.
              '';
            };
            disruptionBudgets = mkOption {
              type = types.listOf budgetType;
              default = [];
            };
            healthChecks = mkOption {
              type = types.attrsOf healthProbeType;
              default = {};
              description = ''
                Fleet-scoped probe declarations (RFC-0010 §3.2 + §4).
                Applies to every host in the fleet, unless overridden
                at the tag or host scope. Operator pattern:

                  # Every host runs heartbeat
                  nixfleet.healthChecks.heartbeat = {
                    kind = "http";
                    url = "http://localhost/health";
                    intervalSeconds = 30;
                    mode = "observe";
                  };

                Resolved at fleet-eval time into each host's effective
                set with precedence host > tag > fleet; rendered into
                the host's closure as
                `/etc/nixfleet/agent/health-checks.json` (transitively
                signed via the closure hash chain — no manifest schema
                expansion).
              '';
            };
            revocations = mkOption {
              type = types.listOf revocationType;
              default = [];
              description = ''
                Operator-declared agent-cert revocations. The release
                pipeline signs these alongside `fleet.resolved` so the
                CP can rebuild `cert_revocations` from empty state
                without a security regression. Empty list is the
                steady state - it still gets signed so a CP rebuild
                has a verifiable source.
              '';
            };
            bootstrapNonces = mkOption {
              type = types.listOf bootstrapNonceType;
              default = [];
              description = ''
                Operator-declared allowlist of valid bootstrap-token
                nonces. Closes the replay-after-DB-wipe vector
                (nixfleet#96): CP refuses /v1/enroll whose nonce is
                not in this signed list. Empty list = no enrolments
                accepted.

                Operator workflow:
                  1. `nixfleet mint-token --hostname X ...` (prints
                     a Nix snippet)
                  2. Paste snippet here, commit, push
                  3. CI signs the sidecar `bootstrap-nonces.json`
                  4. CP polls + applies (within 60s)
                  5. Deploy token to host, agent enrolls

                Entries can be left in this list as an audit log;
                `nixfleet-release` filters out entries with
                `expiresAt` in the past at sign time.
              '';
            };
          };
        }
        input
      ];
    };
  in
    evaluated.config
    // {
      resolved = resolveFleet evaluated.config;
      revocations = evaluated.config.revocations;
      bootstrapNonces = evaluated.config.bootstrapNonces;
    };

  # LOADBEARING: hosts/tags/channels strict-merge (collision throws); rolloutPolicies later-wins; edges/channelEdges/disruptionBudgets concat.
  mergeFleets = fleetInputs: let
    mergeStrict = kind: a: b:
      lib.foldl' (
        acc: name:
          if acc ? ${name}
          then throw "mergeFleets: ${kind} '${name}' is defined in multiple inputs"
          else acc // {${name} = b.${name};}
      )
      a (lib.attrNames b);
    step = acc: input: {
      hosts = mergeStrict "host" acc.hosts (input.hosts or {});
      tags = mergeStrict "tag" acc.tags (input.tags or {});
      channels = mergeStrict "channel" acc.channels (input.channels or {});
      rolloutPolicies = acc.rolloutPolicies // (input.rolloutPolicies or {});
      edges = acc.edges ++ (input.edges or []);
      channelEdges = acc.channelEdges ++ (input.channelEdges or []);
      disruptionBudgets = acc.disruptionBudgets ++ (input.disruptionBudgets or []);
    };
    empty = {
      hosts = {};
      tags = {};
      channels = {};
      rolloutPolicies = {};
      edges = [];
      channelEdges = [];
      disruptionBudgets = [];
    };
    merged = lib.foldl' step empty fleetInputs;
  in
    mkFleet merged;
in {
  inherit mkFleet mergeFleets withSignature;
}
