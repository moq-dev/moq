# [M] moq-transport

## Goal

The grant and refresh exchange works on a moq-transport session between two
moq-net peers, so `Session::auth()` behaves the same over either wire and an
IETF publisher fails loud on an out-of-scope PUBLISH_NAMESPACE before sending
it. A new `drafts/draft-lcurley-moq-auth.md` specifies it as an extension a
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
- One long-lived AUTH request stream per direction, the way a subscribe
  request stream outlives its SUBSCRIBE_OK. The first AUTH carries a Request
  ID like every request; every AUTH also carries the lite Sequence, and
  AUTH_OK and AUTH_ERROR echo it, with Sequence 0 for an unprompted update.
  Correlation is the Sequence, never the Request ID, which the allocator
  hands out from zero and so cannot double as an update marker. Track
  namespaces are tuples on this wire, so a prefix is a namespace tuple,
  matching how SUBSCRIBE_NAMESPACE spells one. Sent only after negotiation;
  an endpoint that receives one without negotiating closes with
  PROTOCOL_VIOLATION, which is what moq-net already does for an unknown
  request stream.
- Which existing codes AUTH_ERROR reuses: `UNAUTHORIZED`, `EXPIRED_AUTH_TOKEN`,
  `MALFORMED_AUTH_TOKEN` from the request error registry.
- A note relating it to the AUTHORIZATION TOKEN setup option: a token
  presented there is the connection credential an empty AUTH refers to.

Cite [moq-wg #1854](https://github.com/moq-wg/moq-transport/issues/1854) in
the introduction: the grant answers which role a peer will play. Run
`just drafts check`; `doc/.vitepress/drafts.ts` discovers the file by name.

### Code

`rs/moq-net/src/ietf/auth.rs` beside `solicit.rs` and `cluster.rs`: the setup
option round trip with the tri-state `from_setup` shape solicit uses, the
three messages as `Message` impls with IDs from the draft, and negotiation
recorded on the peer state. `run_dispatch` in `ietf/session.rs` routes an AUTH
request stream to the shared `auth::Handle` from
[Lite stream](/quest/m2/auth/lite.md); `ietf::start` opens one after SETUP
when negotiated and sends the empty AUTH, and answers the peer's from the
origin handles exactly as lite does. The fail-loud check moves into the shared
handle so the IETF subscriber half consults it before writing
PUBLISH_NAMESPACE, aborting with `Unauthorized` and the path.

`js/net/src/ietf/auth.ts` mirrors it, wired through `handshake.ts` like
`Ietf.Cluster.intoSetup` and `fromSetup`, and `ietf/connection.ts` dispatches
the request stream.

Version-gate on draft-17+; earlier drafts leave `grant()` at `None` and
`refresh` at `Unsupported`.

### Tests

Setup option round trip on every supported draft and absence on 14 to 16;
negotiation requires both sides; grants from scoped origins over an IETF
session in Rust, JS, and across; an out-of-scope path aborts before any
PUBLISH_NAMESPACE is written; a peer without the option (the relay built
without it, and the interop runner's reference relay) sees no AUTH stream and
keeps working. Run `just test smoke-full`.

On main, additive.

## Required

- [Lite stream](/quest/m2/auth/lite.md) - supplies `moq_net::auth` and the
  shared handle this binds to the IETF wire

## Related

- [IETF error codes](/quest/m0/ietf-error-codes.md) - the registered codes
  AUTH_ERROR reuses
