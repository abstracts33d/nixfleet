# `nixfleet_control_plane::auth_cn`

mTLS defense-in-depth: extract verified peer cert + CN, expose as
request extension, optionally enforce CN-vs-path-id.

Ported from v0.1's `crates/control-plane/src/auth_cn.rs` (tag
v0.1.1) with the upstream-attribution comments preserved. v0.1
shipped against the same axum-server 0.7 + tokio-rustls 0.26 +
x509-parser 0.16 stack, so the implementation is drop-in.

## Why this module exists in-tree

`axum-server 0.7.3` does not expose peer certificates through any
public API ([upstream issue #162](https://github.com/programatik29/axum-server/issues/162)).
The standard fix is a custom `Accept` wrapper that, after the TLS
handshake, reads `tokio_rustls::server::TlsStream::get_ref().1.peer_certificates()`
and injects the chain into every request on that connection via a
per-connection `tower::Service` wrapper.

The `axum-server-mtls` crate (v0.1.0) implements exactly this
pattern. We vendor a trimmed-down version in-tree to avoid taking
a 0.1.0 third-party dependency. The implementation is mechanical
and matches the upstream pattern.

## Wiring

`server.rs` builds the `RustlsAcceptor`, wraps it in
`MtlsAcceptor::new(...)`, and calls
`axum_server::bind(addr).acceptor(mtls)`. The `MtlsAcceptor`
injects `PeerCertificates` into every request extension on a
connection. The `/v1/whoami` handler reads the extension via
[`PeerCertificates::leaf_cn`]. The future
[`cn_matches_path_machine_id`] middleware (wired by PR-3+ on
agent-facing routes that take a `{id}` path segment) reads the
extension and rejects with 403 if the CN does not match.

When mTLS is not configured (`tls.client_ca` is None at the
server config level), the `PeerCertificates` extension still
exists but is empty (`is_present() == false`). The middleware
treats that as a no-op and lets the request through, so PR-1's
TLS-only mode (and the existing /healthz integration test) keeps
working.

## Items

### 🔓 `struct PeerCertificates`

Client certificate chain extracted from the TLS connection,
injected into every request as an extension by [`MtlsAcceptor`].
If the client did not present a certificate the chain is empty.


### `impl PeerCertificates`

- **`leaf_cn`** — Extract the Common Name from the leaf certificate's subject.
Returns `None` if no certificate is present or the CN cannot
be parsed.
- **`leaf_not_before`** — Extract the leaf certificate's `notBefore` as UTC. Used by
the revocation check: a revocation entry says "any cert with
notBefore < X is bad", so a re-enrolled cert (with
notBefore > X) re-grants access.

### 🔓 `struct MtlsAcceptor`

Wraps a [`RustlsAcceptor`] so that the peer certificate chain is
extracted after the TLS handshake and injected into every request
on that connection via a per-connection [`PeerCertService`]
wrapper.

Built from an existing `RustlsAcceptor` so the operator's TLS
config (cert, key, optional client CA) flows through unchanged.


### 🔓 `struct PeerCertService`

Per-connection [`tower::Service`] wrapper that injects
[`PeerCertificates`] into every request's extensions. Constructed
internally by [`MtlsAcceptor::accept`]; not meant to be built by
hand.


### 🔓 `fn cn_matches_path_machine_id`

Middleware applied to agent-facing routes that take a `{id}` path
segment. Extracts the [`PeerCertificates`] injected by
[`MtlsAcceptor`], reads the leaf CN, and rejects with 403 if it
does not match the path id.

No-op when:
- The request has no `PeerCertificates` extension at all (e.g.
  the integration test harness uses raw `axum::serve` over a TCP
  listener with no TLS layer).
- The `PeerCertificates` extension is present but empty (mTLS is
  not configured at the server level).

Both no-op cases let the request through unchanged so PR-1's
TLS-only mode keeps working. Agent-route wiring lands in PR-3
when `/v1/agent/checkin` and `/v1/agent/report` go in.


