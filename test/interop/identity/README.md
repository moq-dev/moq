# Identity proof candidate

This is an unactivated, test-only proposal for
[Identity proof](/quest/m1/cluster-routing/identity.md). Nothing dispatches these
messages on a production AUTH stream. The Rust module is `cfg(test)` and the
WebCrypto module is excluded from the published JS build. Both consume
`vectors.json`; its private scalars are public test keys, never credentials.

The candidate uses P-256 ECDSA with SHA-256, canonical 65-byte uncompressed SEC1
public keys and 64-byte P1363 signatures. The identity is all 256 bits of
`SHA256("moq-routing-identity-v1\0" || public_key)`, separate from legacy Hop.
Rust uses the maintained RustCrypto implementation already used by moq-auth;
JS uses WebCrypto with a nonextractable private key and one pending generation
promise shared by the tab. This fixture models a process singleton in Rust,
not the eventual connection API.

The provisional frames are `AUTH_CNF = 2 || 97 || public_key[65] || nonce[32]`
and `AUTH_PROOF = 3 || 64 || signature[64]`. All four leading integers fit the
lite-07 leading-ones one-byte encoding. They are proposals, not registered
AuthRequest variants. The role-specific signed transcript is
`"moq-lite-07-wip identity-proof-v1\0" || role[1] || has_binding[1] ||
client_key[65] || client_nonce[32] || server_key[65] || server_nonce[32] ||
optional_binding[32]`. Role 0 proves the client key; role 1 proves the server
key. Proposed order is both CNFs, then each role's proof, then own-identity
routing. Exact stream dispatch, close behavior and token-refresh coexistence
remain unimplemented. Identity possession grants no namespace rights.

The slot owns the local challenge and optional binding, reserves one attempt
before peer curve validation and never reopens after success or failure. JS
snapshots all mutable byte inputs before yielding. Tests reject no-proof and
mismatched identity, replay against a new challenge or binding, reflection,
key swaps, duplicate proofs and malformed fixed-width fields. Same static key
at both ends is allowed. These are fixture invariants, not production AUTH
ordering or live network CPU-bound evidence.

The executable counterexample relays both honest CNFs and both signatures
across two different sessions. Nonce-only ownership succeeds without either
private key. This demonstrates relayed identity acceptance; there is no
production ROUTE handler in this fixture. A locally held authenticated
transport binding rejects the relayed proof, provided the transport actually
supplies distinct, agreed binding bytes.

The WebCrypto alternative signs both ephemeral P-256 ECDH keys, derives
separate directional HMAC-SHA256 keys through HKDF-SHA256, and authenticates
opaque own-control bytes together with identity, seqno and an ordered u64
counter. The receiver reads the counter from the authenticated frame, owns
one verification at a time, snapshots mutable inputs and refuses counter
exhaustion before wrap. Tests accept honest forwarded controls while refusing
seqno edits, keyless MAC forgery, reflected directions, serial/concurrent
replay and rewritten counter metadata, including shared static keys. This is
feasibility evidence, not a proposed final ROUTE format: every authority-changing
control, including ANNOUNCE/END and revisions, must be covered before this
alternative can enforce the quest's goal.

Run the Rust cases with `just rs test -p moq-net --lib identity_fixture`, and
Bun with `bun test js/net/src/lite/identity.test.ts`. The same WebCrypto cases
run in real Chromium through `just test harness`, already selected by CI and
nightly. The vectors verify Rust signatures in WebCrypto and WebCrypto
signatures in Rust; they do not establish production AUTH interoperability.

## Measured transport capability

Read-only probes on 2026-10-22 used Playwright 1.64.1, a secure localhost page
and normal runner flags. Chromium 156.0.8078.4 exposed WebTransport but no
`exportKeyingMaterial`; Firefox 157.0 exposed the method, without a live-session
agreement test. WebKit failed while creating a page, so its capability is
unknown. Node 24.21.0 and Bun 1.4.2 had no native WebTransport; installing the
existing @moq/web-transport 0.1.5 polyfill supplied it without an exporter.
Current Established and transport adapters expose no common exporter.

The [WebTransport standard](https://www.w3.org/TR/webtransport/) describes the
API, while [HTTP/3 WebTransport section 4.8](https://datatracker.ietf.org/doc/html/draft-ietf-webtrans-http3#section-4.8)
binds it to the CONNECT session as well as TLS. Raw TLS exporter bytes are not
an interchangeable implementation. Chromium still lists the feature as
[experimental](https://chromium.googlesource.com/chromium/src/+/master/third_party/blink/renderer/platform/runtime_enabled_features.json5);
Firefox's implementation is [fixed for 155](https://bugzilla.mozilla.org/show_bug.cgi?id=2007200),
and WebKit has a [backend implementation](https://commits.webkit.org/317835@main).
Method presence alone proves neither browser coverage nor native/browser
agreement. The decision is transport capability and mapping versus a fully
authenticated application routing channel, not whether WebCrypto can sign.
