# `nixfleet_reconciler`

Pure-function rollout reconciler + RFC-0002 §4 step 0 verification.

Two public entry points, intentionally decoupled:

- [`verify_artifact`] — step 0: parse + canonicalize + signature-verify
  + freshness-check a `fleet.resolved.json` artifact. Returns a verified
  [`FleetResolved`] or a [`VerifyError`].
- [`reconcile`] — steps 1–6: pure decision procedure. Takes a verified
  [`FleetResolved`], an [`Observed`] state, and `now`; returns
  `Vec<`[`Action`]`>`.

The CP tick loop calls them in sequence. Tests exercise each
independently. Both are stateless: state lives in the inputs.

## Items

### 🔓 `mod action`

_(no doc comment)_


### 🔓 `mod observed`

_(no doc comment)_


### 🔓 `mod reconcile`

_(no doc comment)_


### 🔓 `mod verify`

_(no doc comment)_


