# [M] moq-transport

## Goal

The grant exchange works on a moq-transport session between two moq-net
peers, with the same grant lifecycle through `Session::auth()`. This initial
IETF encoding supports prefix-representable grants and explicitly refuses
other pattern unions. An IETF publisher fails loud on an out-of-scope
PUBLISH_NAMESPACE before sending it. A
new `drafts/draft-lcurley-moq-auth.md` specifies it as an extension a
conforming peer can ignore.

## Plan

### Draft

`drafts/draft-lcurley-moq-auth.md`, modeled on `draft-lcurley-moq-solicit.md`
for the setup option and on `draft-lcurley-moq-cluster.md` for the IANA
tables. It declares:

- Setup Option `AUTH`, an even key in the `0x40B5x` series the cluster and
  solicit options use, value `1`. Both endpoints send it; the extension is
  negotiated only when both did, per the moqt extension rule that the set is
  fixed once both SETUPs are seen. Draft-17 and later only, since that is the
  first unified SETUP, the same gate `cluster::supported` applies.
- One AUTH request stream per token, living as long as the token, the way a
  subscribe request stream outlives its SUBSCRIBE_OK. AUTH carries a Request
  ID like every request plus the token; AUTH_OK and AUTH_ERROR carry the lite
  fields and may repeat on the stream with the lite update and revoke
  meanings; closing the stream withdraws the token. Track namespaces are
  tuples on this wire, so a prefix is a namespace tuple, matching how
  SUBSCRIBE_NAMESPACE spells one. Sent only after negotiation; an endpoint
  that receives one without negotiating closes with PROTOCOL_VIOLATION, which
  is what moq-net already does for an unknown request stream.
- Which existing codes AUTH_ERROR reuses: `UNAUTHORIZED`, `EXPIRED_AUTH_TOKEN`,
  `MALFORMED_AUTH_TOKEN`, and `NOT_SUPPORTED` from the request error registry.
  An unrepresentable pattern grant returns `NOT_SUPPORTED`, surfaced by the
  public handle as `Unsupported`, rather than widening it into a namespace
  prefix. The shared request error mapping already supports that code.
- A note relating it to the AUTHORIZATION TOKEN setup option: a token
  presented there is the connection credential an empty AUTH refers to.

Cite [moq-wg #1854](https://github.com/moq-wg/moq-transport/issues/1854) in
the introduction: the grant answers which role a peer will play. Run
`just drafts check`; `doc/.vitepress/drafts.ts` discovers the file by name.

### Grant conversion

Keep the public grant pattern-valued. Convert only unions that can be expressed
exactly as namespace-prefix tuples; a subtree and the all-path grant are
representable, while exact-path, suffix, and segment-wildcard grants are not.
Validate the whole publish/subscribe union before emitting AUTH_OK. Never
silently drop a member, broaden it to a literal head, or leave the request
waiting indefinitely. An unsupported initial grant refuses that token; an
unsupported update revokes its previous grant with AUTH_ERROR and closes the
stream. Other tokens and work still covered by them remain on the session.

Full-pattern IETF encoding would require a separately negotiated extension
revision and is outside this initial prefix-tuple contract. Document this
capability limit in the draft and token-in-band guidance.

### Code

`rs/moq-net/src/ietf/auth.rs` beside `solicit.rs` and `cluster.rs`: the setup
option round trip with the tri-state `from_setup` shape solicit uses, the
three messages as `Message` impls with IDs from the draft, and negotiation
recorded on the peer state. `run_dispatch` in `ietf/session.rs` routes an AUTH
request stream to the shared `auth::Handle` from
[Lite stream](/quest/next/auth/lite.md); `ietf::start` opens the empty-token
stream after SETUP when negotiated and answers the peer's from the origin
handles exactly as lite does, and `add` opens further ones. The fail-loud
check moves into the shared handle so the IETF subscriber half consults it
before a new PUBLISH_NAMESPACE, aborting with `Unauthorized` and the path.
A shrinking grant instead withdraws previously authorized publications and
cancels only affected subscriptions, preserving the session as lite does.

`js/net/src/ietf/auth.ts` mirrors it, wired through `handshake.ts` like
`Ietf.Cluster.intoSetup` and `fromSetup`, and `ietf/connection.ts` dispatches
the request streams.

Version-gate on draft-17+; earlier drafts leave `grant()` at `None` and `add`
at `Unsupported`.

### Tests

Cross-language fixtures cover prefix unions and the root grant, refusal of
exact/suffix/segment-wildcard and mixed unions, plus an update from a supported
grant to an unsupported one that revokes the old permission without changing
other tokens. Assert `Unsupported` completes promptly and no unauthorized
PUBLISH_NAMESPACE is sent.

Setup option round trip on every supported draft and absence on 14 to 16;
negotiation requires both sides; grants from scoped origins over an IETF
session in Rust, JS, and across; two tokens union and closing one shrinks the
union without disconnecting, cancelling only work that loses authorization;
an out-of-scope new publication aborts before any PUBLISH_NAMESPACE is written; a
peer without the option (the relay built without it, and the interop runner's
reference relay) sees no AUTH stream and keeps working. Run
`just test smoke-full`.

On main, additive.

## Required

- [Lite stream](/quest/next/auth/lite.md) - supplies `moq_net::auth` and the
  shared handle this binds to the IETF wire
