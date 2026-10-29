# [XS] moq-transport 18, 20, and 22 only

## Goal

`moq-net` and `@moq/net` speak moq-transport drafts 18, 20, and 22 and no
others. Drafts 14 to 17, 19, and 21 are deleted from the wire code, tests,
fuzzing, interop lanes, and docs, and our `moq-dev-rs` and `moq-dev-js`
entries in the interop runner registry declare only 18, 20, and 22.

Out of scope: moq-lite versions. moq-lite-01/02 still negotiate over
`moql` with the CLIENT_SETUP/SERVER_SETUP codec, which stays.

## Plan

Decided with the maintainer on 2026-10-24. moq-transport support matters
only for interop with third-party implementations; our own clients and
servers always prefer moq-lite. A draft stays only when the peers it buys
are worth its code (cost-weighted, not "any live peer").

Peers per draft, from `englishm/moq-interop-runner` `implementations.json`
on 2026-10-24:

- 18: 15 third-party peers, the most deployed draft.
- 22: 8 peers (aiomoqt, imquic, moq-go, moqtail, Nokia, moxygen, red5, moqxr).
- 14: aiomoqt, Cloudflare's draft-14 relay, xquic. Cloudflare and xquic also
  run draft-18 builds.
- 16: moxygen (its recommended draft; its 18 is experimental), Cloudflare
  `moq-rs` main (16 only), and others that also speak 18.
- 15: none. 17: imquic and quic-zig, both also on 18. 19 and 21: imquic
  only. 20: aiomoqt and imquic, both also on 22.

So dropping 15, 17, and 19 to 21 loses no peer. Dropping 14 and 16 loses
Cloudflare `moq-rs` main and moves moxygen onto its experimental draft-18;
the maintainer accepted that for the savings (the cheaper alternative,
keeping 16, 18, and 22, was declined).

Why 14 to 16 cost the most, measured on `main` at `28bd16452d`:

- `ietf/adapter.rs` (2,701 lines) and `js/net/src/ietf/adapter.ts`
  (1,211) exist only to multiplex 14-16 requests on one control stream.
  74 commits touched them since 2026-08-01, 44 in October.
- At least 11 fixes since August were 14-16 only, such as
  [moq-dev/moq#2664](https://github.com/moq-dev/moq/pull/2664),
  [moq-dev/moq#3481](https://github.com/moq-dev/moq/pull/3481),
  [moq-dev/moq#4961](https://github.com/moq-dev/moq/pull/4961),
  [moq-dev/moq#5798](https://github.com/moq-dev/moq/pull/5798),
  [moq-dev/moq#6051](https://github.com/moq-dev/moq/pull/6051), and
  [moq-dev/moq#6133](https://github.com/moq-dev/moq/pull/6133).
- Cross-cutting IETF features also land a second path through the adapter,
  for example GOAWAY
  ([moq-dev/moq#5998](https://github.com/moq-dev/moq/pull/5998)) and watched
  prefixes ([moq-dev/moq#6530](https://github.com/moq-dev/moq/pull/6530)).

m1, ranked beside the API audit, because the drop deletes public API
(`session::Limits::requests` and the relay's request limit) and belongs with
the other breaks before the cut. Drafts 14 to 17
([moq-dev/moq#6753](https://github.com/moq-dev/moq/pull/6753)) and 19 to 21
([moq-dev/moq#6754](https://github.com/moq-dev/moq/pull/6754)) are gone.

Decided 2026-10-28 (maintainer): draft 20 came back, so the public relay
dashboard lists more than two moq-transport drafts. It costs only draft-20's
field-count LOCATION_FILTER codec and its SUBSCRIBE_TRACKS parameters; every
other gate is shared with draft 22, whose changes since 20 are editorial
(draft-22 appendix A.1 and A.2). Drafts 19 and 21 stay dropped: 19 splits
most gates between 18 and 22, and 21 differs from 20 only by its ALPN.

This README's remaining work: open a PR on
`englishm/moq-interop-runner` narrowing the `moq-dev-rs` and `moq-dev-js`
entries in `implementations.json` to drafts 18, 20, and 22. The maintainer
approved posting there on 2026-10-24.

## Related

- [Interop coverage](/quest/m1/interop-coverage/README.md) - new cross-implementation cases target drafts 18 and 22 only
