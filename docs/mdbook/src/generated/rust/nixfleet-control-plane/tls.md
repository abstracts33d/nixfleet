# `nixfleet_control_plane::tls`

TLS server config builder.

Ported from v0.1's `crates/control-plane/src/tls.rs` (tag v0.1.1).
v0.1 shipped this against the same rustls 0.23 + rustls-pki-types 1
stack we're now re-adopting in Phase 3, so the implementation is
drop-in. PR #36 removed the entire wire surface when Phase 2's
reconciler runner had no listening socket; this re-introduces the
TLS-only path for PR-1. PR-2 layers `WebPkiClientVerifier` on top
for mTLS — the `client_ca_path` parameter already plumbs through
so no signature change is needed when that lands.

## Items

### 🔓 `fn build_server_config`

Build a rustls `ServerConfig`. When `client_ca_path` is `Some`, the
returned config requires verified client certs signed by the CA at
that path (mTLS). When `None`, the listener accepts any client
without authentication — appropriate for PR-1 + `/healthz` only;
PR-2 onwards always pass a CA path.

All file IO is synchronous and happens once at startup. Failures
here should crash the process — they indicate misconfigured agenix
paths or a damaged fleet CA, neither of which the runtime can
recover from.


