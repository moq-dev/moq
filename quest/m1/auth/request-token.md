# [L] An experimental Rust hook verifies a request token

## Goal

Rust applications can opt into verifying a moq-transport token on SUBSCRIBE
or PUBLISH_NAMESPACE and renewing that request's grant in band. This is a
temporary interoperability hook, not a complete authentication system or
relay feature. The application supplies verification and policy. No request
token becomes session authority, and a peer's AUTH_OK cannot authorize its
incoming requests.

The hook retains renewal, expiry, revocation, and dependent joining FETCH.
Per-request relay integration, JS setters and acceptors, CLI flags, and
bindings are deferred until a consumer needs them. Session AUTH work remains
separate.

## Plan

Continue [#5148](https://github.com/moq-dev/moq/pull/5148), which carries
[#4675](https://github.com/moq-dev/moq/pull/4675) forward. Decisions confirmed
2026-10-12:

- **Scope.** Keep the Rust hook and renewal instead of reducing the work to
  decoding or expanding it into relay integration. Interoperability is the
  reason to expose it; carrying a token alone does not authenticate it.
- **Verifier API.** Keep one event queue with explicit session/request
  variants and distinct acceptance handles. A session-token consumer must
  consciously handle or refuse the request variant; an optional `scope()`
  on a shared `accept()` is insufficient. The types must distinguish a
  session grant advertised to a peer from a locally enforced request grant.
  Update existing consumers together, including the relay's explicit refusal
  of request verification. This intentionally changes the acceptor API so
  existing session handlers cannot silently acquire new security semantics.
- **Admission.** Independently verified local session permission takes
  precedence. Only when it does not cover the request is the request token
  verified, with the accepted grant checked against that request's path and
  kind. Grants received in AUTH_OK describe what the peer allows us to do;
  they never skip verification of what the peer asks us to do. Without local
  permission or a verified request grant, refuse `UNAUTHORIZED`; without an
  application request verifier, refuse a token requiring verification as
  `Unsupported`. Malformed token encodings still fail before admission.
- **Lifetime and bounds.** Pending initial verification consumes request
  capacity before awaiting the application and observes request/session
  cancellation. Withdrawal must cancel pending work and make late answers
  ineffective; any verifier work still in flight remains counted until it
  ends. Renewal has the same ownership discipline. Coordinate queue
  bounds and cancellation with [Relay tokens](/quest/m1/auth/relay-refresh.md)
  and #5282, which already adds shared pending-AUTH support; do not defer the
  initial-admission fix to the later request-gate refactor. Reuse shared
  primitives whichever PR lands first, without approving #5282's separate
  API or limit-policy decisions here.
- **Documentation.** Minimal Rust API comments only: experimental hook,
  application verification required, no per-request relay integration, and
  setter success is not peer authorization. No feature page, guide, or guide
  quest. Keep required wire registries synchronized with changed codes.

Retained decisions:

- **Wire scope.** Honor tokens on SUBSCRIBE, unsolicited PUBLISH_NAMESPACE,
  and their renewal updates; other request tokens grant no authority. The
  solicited NAMESPACE response has no request-token slot. Preserve the
  decoder rules landed by [#5107](https://github.com/moq-dev/moq/pull/5107),
  including session-fatal malformed/forbidden forms and request-level
  refusal of repeated identical tokens. SETUP retains its own alias rules.
- **Renewal across drafts.** SUBSCRIBE_UPDATE renews subscriptions on drafts
  14 and 15; REQUEST_UPDATE renews subscriptions and namespaces from 16.
  Fix #5148's missing draft-16 namespace renewal, including Rust/JS decoding
  and refusal of malformed, forbidden, or repeated tokens on that path.
- **Request grants stay local.** A grant covers only its request, never
  joins a session union, and ends with the request. A refused renewal ends
  only that request and does not retain the old grant: REQUEST_ERROR and
  PUBLISH_DONE `UPDATE_FAILED` for a subscription, a closed namespace stream
  from draft 17, or PUBLISH_NAMESPACE_CANCEL on draft 16. Expiry and revocation
  also end only the affected request.
- **Joining FETCH.** Inherit the named subscription's live authorization,
  confined to its pinned track and allowed range, observing renewal,
  revocation, expiry, and the session ceiling. End with that subscription;
  neither a cached namespace nor a grant snapshot prolongs authority.
  Standalone FETCH gains no request-token authorization.
- **Client credential.** Keep `auth::Handle::set_request_token` and typed
  `setup::Token`, encoding only `USE_VALUE`; defer aliases. It is a default
  for outgoing requests on the connection, not a token chosen individually
  for each request. Replacement renews live eligible requests, and success
  means local configuration accepted, not authentication acknowledged.
  `Connection::auth()` owns it across reconnects and
  `connect::Config::with_request_token` seeds it. Share the persistent handle
  and server passthrough with #5282; add no parallel `Client` methods.
- **Update credit.** Advertise and enforce MAX_REQUEST_UPDATES on draft 19+,
  with TOO_MANY_REQUEST_UPDATES (0x1B) in the shared Rust/JS session registry
  and moq-lite's registry. Earlier drafts keep a local guard that ends only
  the request. Keep one renewal in flight where updates receive answers;
  draft 14 must permit successive replacements without waiting for answers.
- Typed `EXPIRED_AUTH_TOKEN` and received `MALFORMED_AUTH_TOKEN` errors remain
  with [Expired token error](/quest/m1/auth/expired-error.md); preserving the
  existing decoder's emitted refusal does not wait on that quest.

Verification belongs in existing CI suites. Reproduce the audit's covering
peer AUTH_OK with a local verifier that refuses: the verifier must run and
the incoming request must fail. Cover both request kinds and the independent
local-session permission case. Exercise stalled initial verification at the
existing request cap, cancellation, late completion, and capacity release.
Preserve tests for scoped grants, renewal refusal, expiry/revocation, two
draft-14 replacements, draft-16 namespace renewal, and token-only joining
FETCH on drafts 14-19 plus native fill on newer drafts. Keep unsupported
protocols explicit. Run scoped checks, wire-draft validation when applicable,
and cross-language interop for the wire changes.

Public API: a breaking Rust acceptor event/type distinction, plus the
request-token setter and persistent connection integration. Export only the
types consumers need; do not keep the optional-scope compatibility shape.
JS receives necessary decoder/registry parity, not a request-token API.
Wire: existing 0x03 tokens and draft-specific renewal messages, plus
MAX_REQUEST_UPDATES (0x08) and TOO_MANY_REQUEST_UPDATES (0x1B) for draft 19+;
0x1B also joins moq-lite's registry. No complete authentication scheme is
promised by these mechanisms.

## Related

- [#5106](https://github.com/moq-dev/moq/pull/5106) - landed setup extension
  configuration; no remaining prerequisite for starting this hook
- [Relay tokens](/quest/m1/auth/relay-refresh.md) - shared pending-AUTH and
  persistent connection plumbing, with session verification kept separate
- [Request leases](/quest/m3/auth-request-lease.md) - deferred relay
  verification, not a prerequisite for this experimental hook
