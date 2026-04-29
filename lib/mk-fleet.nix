# lib/mk-fleet.nix
#
# Produces `fleet.resolved` per RFC-0001 §4.1 + docs/CONTRACTS.md §I #1.
# Output is canonicalized to JCS (RFC 8785) by `bin/nixfleet-canonicalize`
# before signing — DO NOT introduce floats, opaque derivations, or
# attrsets whose iteration order is significant here.
{lib}: let
  inherit (lib) mkOption types;

  # --- Selector algebra (RFC-0001 §3) ---
  # Variants evaluated in precedence order: `not` > `and` > base OR over
  # (tags, tagsAny, hosts, channel, all). `not` and `and` are recursive —
  # selectors compose to arbitrary set algebra.
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

  # --- Host ---
  hostType = types.submodule {
    options = {
      system = mkOption {type = types.str;};
      configuration = mkOption {
        type = types.unspecified;
        description = "A nixosConfiguration.";
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
    };
  };

  # --- Revocations (gap C of docs/roadmap/0002-v0.2-completeness-gaps.md) ---
  # Operator-declared agent-cert revocation entries. The release
  # pipeline serialises this list, signs it with the same
  # ciReleaseKey that signs `fleet.resolved`, and writes
  # `revocations.json` alongside `fleet.resolved.json`. The CP
  # fetches + verifies the signed sidecar on each reconcile tick
  # and replays entries into `cert_revocations`. An empty list is
  # the steady state — it still gets signed so a CP rebuilt from
  # empty has a verifiable source for the (empty) revocation set.
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

  tagType = types.submodule {
    options.description = mkOption {
      type = types.str;
      default = "";
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

  channelType = types.submodule {
    options = {
      description = mkOption {
        type = types.str;
        default = "";
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
      compliance = mkOption {
        type = types.submodule {
          options = {
            # Issue #58 — tri-state policy mode shared by the static
            # gate (this file) and the runtime gate (#57 agent-side).
            # `null` falls back to the legacy `strict` mapping below
            # so existing fleets with `strict = true|false` keep
            # working unchanged.
            mode = mkOption {
              type = types.nullOr (types.enum ["disabled" "permissive" "enforce"]);
              default = null;
              description = ''
                Compliance gate policy. When set, overrides the legacy
                `strict` field for both the static gate (mk-fleet eval)
                and the runtime gate (agent post-activation).

                - `disabled`: gate not run.
                - `permissive`: failing static evidence emits a
                  `lib.warn` per failing host/control; eval succeeds.
                - `enforce`: failing static evidence throws at fleet
                  eval. Same wire-shape as `strict = true`.

                When `null` (default), behaviour is derived from
                `strict`: `true → enforce`, `false → disabled`. Set
                `mode` explicitly to opt into permissive.
              '';
            };
            strict = mkOption {
              type = types.bool;
              default = true;
              description = ''
                Legacy boolean form of `mode` (issue #58). Kept for
                backward compatibility with existing fleets and the
                wire-level `Compliance.strict: bool` consumed by the
                Rust control plane. Prefer `mode` for new code.

                When `mode` is set, this field is ignored for gate
                decisions but still flows through to the resolved
                output (computed from the effective mode:
                `enforce → true`, otherwise `false`).
              '';
            };
            frameworks = mkOption {
              type = types.listOf types.str;
              default = [];
            };
          };
        };
        default = {};
      };
    };
  };

  edgeType = types.submodule {
    options = {
      before = mkOption {type = types.str;};
      after = mkOption {type = types.str;};
      reason = mkOption {
        type = types.str;
        default = "";
      };
    };
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

  # Tarjan-free cycle detection using iterative DFS marking.
  # Edges: { after = "a"; before = "b"; } means a must finish before b starts.
  # So we walk "after → before" edges.
  hasCycle = edges: let
    adj =
      lib.foldl' (
        acc: e: let
          current = acc.${e.after} or [];
        in
          acc // {${e.after} = current ++ [e.before];}
      ) {}
      edges;
    nodes = lib.unique (map (e: e.after) edges ++ map (e: e.before) edges);
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

  # --- Selector resolution: selector × hosts → [host-name] ---
  # Variant precedence (RFC-0001 §3): `not` > `and` > base OR composition.
  # Base OR = host matches iff any of tags/tagsAny/hosts/channel/all matches.
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

  # --- Invariant checks (RFC-0001 §4.2) ---
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
          lib.optional (!builtins.elem e.before hostNames) "edge.before references unknown host '${e.before}'"
          ++ lib.optional (!builtins.elem e.after hostNames) "edge.after references unknown host '${e.after}'"
      )
      cfg.edges;

    configurationErrors =
      lib.concatMap (
        n: let
          h = cfg.hosts.${n};
          isValid =
            builtins.isAttrs h.configuration
            && h.configuration ? config
            && h.configuration.config ? system
            && h.configuration.config.system ? build
            && h.configuration.config.system.build ? toplevel;
        in
          lib.optional (!isValid)
          "host '${n}' configuration is not a valid nixosConfiguration (missing config.system.build.toplevel)"
      )
      hostNames;

    complianceErrors =
      lib.concatMap (
        channelName: let
          c = cfg.channels.${channelName};
          bad = lib.filter (f: !(builtins.elem f cfg.complianceFrameworks)) c.compliance.frameworks;
        in
          map (f: "channel '${channelName}' references unknown compliance framework '${f}'") bad
      )
      (lib.attrNames cfg.channels);

    cycleErrors = lib.optional (hasCycle cfg.edges) "edges form a cycle; the DAG invariant is violated";

    freshnessErrors =
      lib.concatMap (
        channelName: let
          c = cfg.channels.${channelName};
        in
          lib.optional (c.freshnessWindow < 2 * c.signingIntervalMinutes)
          "channel '${channelName}': freshnessWindow (${toString c.freshnessWindow}) must be ≥ 2 × signingIntervalMinutes (${toString c.signingIntervalMinutes})"
      )
      (lib.attrNames cfg.channels);

    # Resolve the effective compliance mode for a channel, honouring
    # the issue #58 unification: explicit `mode` wins; falling back to
    # the legacy `strict` mapping (`true → enforce`, `false →
    # disabled`). Both fields stay in the schema so existing fleets
    # keep working; new code should set `mode` explicitly.
    resolvedComplianceMode = channelName: let
      c = cfg.channels.${channelName}.compliance;
    in
      if c.mode != null
      then c.mode
      else if c.strict
      then "enforce"
      else "disabled";

    # Compute the (host, control) failure tuples for a channel's
    # static-or-both controls. Shared by the enforce + permissive
    # branches below — the difference between the two is only what
    # we DO with the failures (throw vs. lib.warn).
    staticFailuresForChannels = channelNames: let
      hostsOnChannels =
        lib.filter (n: builtins.elem cfg.hosts.${n}.channel channelNames) (lib.attrNames cfg.hosts);
    in
      lib.concatMap (
        hostName: let
          host = cfg.hosts.${hostName};
          probes = host.configuration.config.compliance.evidence.probes or {};
          probeNames = lib.attrNames probes;
          # Only static + both controls participate in the build-time
          # gate. `runtime`-only controls produce evidence after
          # activation and are gated by the agent / CP at confirm time.
          staticOrBoth =
            lib.filter (
              p: let
                t = probes.${p}.type or "runtime";
              in
                t == "static" || t == "both"
            )
            probeNames;
          failures =
            lib.filter (
              p: let
                ev = probes.${p}.staticEvidence or null;
              in
                ev != null && (ev.passed or true) == false
            )
            staticOrBoth;
          mode = resolvedComplianceMode host.channel;
        in
          map (p: "host '${hostName}' (channel '${host.channel}', ${mode}): static control '${p}' failed — ${lib.generators.toPretty {} (probes.${p}.staticEvidence.evidence or {})}") failures
      )
      hostsOnChannels;

    # Static compliance gate (issue #4 / #58). For every host on a
    # channel whose effective mode is `enforce`, walk
    # `compliance.evidence.probes.*` (populated by the
    # nixfleet-compliance modules each host imports) and collect
    # static/both probes whose `staticEvidence.passed` is explicitly
    # false. Failures throw at fleet-eval time, before CI ever signs
    # a release.
    #
    # Hosts on `permissive` channels emit `lib.warn` per failure but
    # don't block eval — operators can introduce compliance to an
    # existing fleet incrementally. `disabled` skips the gate
    # entirely (no traversal, no warnings).
    enforceChannels =
      lib.filter (n: resolvedComplianceMode n == "enforce") (lib.attrNames cfg.channels);
    staticComplianceErrors = staticFailuresForChannels enforceChannels;

    errs = hostChannelErrors ++ channelPolicyErrors ++ edgeErrors ++ configurationErrors ++ complianceErrors ++ cycleErrors ++ freshnessErrors ++ staticComplianceErrors;
  in
    if errs == []
    then true
    else throw ("nixfleet invariant violations:\n  - " + lib.concatStringsSep "\n  - " errs);

  # --- Resolved projection (RFC-0001 §4.1) ---
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

      # Issue #58 — permissive-mode compliance warnings. Mirrors
      # the staticComplianceErrors accumulator in checkInvariants but
      # selects channels whose effective mode is `permissive` instead
      # of `enforce`. Failures emit `lib.warn` and let eval succeed,
      # so operators see what would fail without breaking
      # `nix flake check`. checkInvariants already ran (via the
      # outer `assert`), so we know the resolved fleet is otherwise
      # valid — this is purely informational.
      compliancePermissiveWarnings = let
        resolveMode = c:
          if c.mode != null
          then c.mode
          else if c.strict
          then "enforce"
          else "disabled";
        permissiveChannels =
          lib.filter (n: resolveMode cfg.channels.${n}.compliance == "permissive") (lib.attrNames cfg.channels);
        hostsOnChannels =
          lib.filter (n: builtins.elem cfg.hosts.${n}.channel permissiveChannels) (lib.attrNames cfg.hosts);
      in
        lib.concatMap (
          hostName: let
            host = cfg.hosts.${hostName};
            probes = host.configuration.config.compliance.evidence.probes or {};
            probeNames = lib.attrNames probes;
            staticOrBoth =
              lib.filter (
                p: let
                  t = probes.${p}.type or "runtime";
                in
                  t == "static" || t == "both"
              )
              probeNames;
            failures =
              lib.filter (
                p: let
                  ev = probes.${p}.staticEvidence or null;
                in
                  ev != null && (ev.passed or true) == false
              )
              staticOrBoth;
          in
            map (p: "[compliance:permissive] host '${hostName}' (channel '${host.channel}'): static control '${p}' failed — ${lib.generators.toPretty {} (probes.${p}.staticEvidence.evidence or {})}") failures
        )
        hostsOnChannels;

      allWarnings = emptySelectorWarnings ++ budgetWarnings ++ compliancePermissiveWarnings;

      # Force the warnings side effect before returning the resolved value.
      # `lib.warn` prints to stderr during eval and returns its second arg.
      emittedWarnings =
        lib.foldl' (acc: msg: lib.warn msg acc) null allWarnings;

      resolved = {
        schemaVersion = 1;
        meta = {
          schemaVersion = 1;
          signedAt = null;
          ciCommit = null;
        };
        hosts =
          lib.mapAttrs (_: h: {
            inherit (h) system tags channel pubkey;
            closureHash = null; # CI fills this in from h.configuration.config.system.build.toplevel
          })
          cfg.hosts;
        channels =
          lib.mapAttrs (_: c: {
            inherit (c) rolloutPolicy reconcileIntervalMinutes signingIntervalMinutes freshnessWindow;
            # Strip `mode = null` from the resolved output so existing
            # JSON goldens (and roundtrip tests in the proto crate)
            # stay byte-identical for fleets that don't opt into the
            # new `mode` field. Non-null modes flow through normally
            # for downstream consumers (CP dispatch, agent gate).
            compliance = lib.filterAttrs (_: v: v != null) c.compliance;
          })
          cfg.channels;
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
        disruptionBudgets =
          map (b: {
            hosts = resolveSelector b.selector cfg.hosts;
            maxInFlight = b.maxInFlight;
            maxInFlightPct = b.maxInFlightPct;
          })
          cfg.disruptionBudgets;
      };
    in
      builtins.seq emittedWarnings resolved;

  # Stamp CI-provided signing metadata onto a resolved fleet value.
  # `signatureAlgorithm` is optional — omit it when signing with ed25519
  # (the default per CONTRACTS §I #1 for backward-compatible consumers).
  # Set it to `"ecdsa-p256"` (or any future value the contract accepts)
  # when CI signs with a non-default algorithm, e.g. when the TPM
  # keyslot emits ECDSA P-256.
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
              type = types.listOf edgeType;
              default = [];
            };
            disruptionBudgets = mkOption {
              type = types.listOf budgetType;
              default = [];
            };
            complianceFrameworks = mkOption {
              type = types.listOf types.str;
              default = ["anssi-bp028" "nis2" "dora" "iso27001"];
              description = ''
                Known compliance frameworks accepted by channel.compliance.frameworks.
                Override only if using an out-of-tree compliance extension.
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
                steady state — it still gets signed so a CP rebuild
                has a verifiable source.
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
    };

  # --- Composition (RFC-0001 §5) ---
  # Merge a list of mkFleet-input attrsets into a single fleet value.
  # Precedence rules:
  #   - hosts / tags / channels: strict merge — same name across inputs throws.
  #   - rolloutPolicies: later wins (associative, not commutative per RFC §5).
  #   - edges / disruptionBudgets: concatenated (no dedup; order preserved).
  #   - complianceFrameworks: union of whatever each input specified; if no
  #     input declared any, the mkFleet default list applies.
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
      disruptionBudgets = acc.disruptionBudgets ++ (input.disruptionBudgets or []);
    };
    empty = {
      hosts = {};
      tags = {};
      channels = {};
      rolloutPolicies = {};
      edges = [];
      disruptionBudgets = [];
    };
    merged = lib.foldl' step empty fleetInputs;
    specifiedFrameworks = lib.concatMap (i: i.complianceFrameworks or []) fleetInputs;
  in
    mkFleet (
      merged
      // lib.optionalAttrs (specifiedFrameworks != []) {
        complianceFrameworks = lib.unique specifiedFrameworks;
      }
    );
in {
  inherit mkFleet mergeFleets withSignature;
}
