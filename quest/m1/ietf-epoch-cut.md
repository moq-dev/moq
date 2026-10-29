# [M] moq-transport carries no epoch

## Goal

The Rust (`rs/moq-net/src/ietf/`) and JS (`js/net/src/ietf/`) moq-transport
sessions no longer offer, honor, or send MoQ Epoch: no EPOCH Setup Option
(`0x40B68`) and no EPOCH parameter (`0x40B69`). Every moq-transport hop is
epochless, as with a third-party peer: namespace alone names the broadcast,
and a publisher restart there reads as Restart. moq-lite 07 keeps carrying
epochs unchanged, so our own relays, which talk moq-lite to each other, keep
them.

Non-goals: `drafts/draft-lcurley-moq-epoch.md` stays as an unimplemented
proposal, and the cluster and hang drafts keep their references to it. No
moq-transport takeover is planned.

## Plan

Decided with the maintainer on 2026-10-28 (`/quest-plan`, "cut epoch over
moq-transport").

Why: no third party implements the extension (a GitHub code search for its
code points finds only this repository), the draft was never submitted to
the datatracker, and the matching and refusal logic already lives in
`rs/moq-net/src/model/origin.rs` and `js/net/src/origin.ts`. What the
moq-transport side adds is about 360 source lines and 580 test lines from
[the feature commit](https://github.com/moq-dev/moq/commit/2dae63603f), much
of it deciding between in-place replacement and DONE plus re-advertising,
since moq-transport has no restart message. It was also the base for an [L]
negotiated takeover, which this cut drops.

Decision prompts:

- Scope: ✅ cut the code and the IETF takeover quest; only drop the takeover
  quest; keep it all. The maintainer first picked a full cut, then kept the
  draft text.
- Shared moq-lite/moq-transport core first, to make epochs cheaper: no. The
  epoch semantics are already shared in the model, so a shared core would
  move the moq-transport-specific replacement code, not delete it. The
  shared serving machines are planned separately in
  [Shared group serving and ingest](/quest/m2/shared-serve.md).
- `setup::Extensions.epoch` (Rust, `deny_unknown_fields`) and JS
  `extensions.epoch`: ✅ delete, as a breaking `feat(net)!`, so a relay
  config still setting `epoch = false` fails loudly at startup; accept and
  ignore.
- Docs: ✅ inline fixes only; a separate docs page.
- Rank: ✅ m1, after the other moq-transport narrowing quests; m2.

Implementation:

- **Rust.** Delete `rs/moq-net/src/ietf/epoch.rs`, the `Peer::epoch` flag,
  the declined-offer bookkeeping in `ietf/peer.rs`, the SETUP option in
  `ietf/session.rs`, and the EPOCH parameter on PUBLISH_NAMESPACE,
  NAMESPACE, SUBSCRIBE, FETCH, and TRACK_STATUS in `ietf/publisher.rs`,
  `ietf/subscriber.rs`, and the message codecs. Delete the replacement
  branches only an epoch-carrying advertisement takes (around `replaces` in
  `ietf/publisher.rs`, and the subscriber's refusal of a namespace that gains
  or loses an epoch). An inbound EPOCH parameter, now never negotiated, is
  handled like any other unknown parameter. Remove `Extensions::epoch`
  from `rs/moq-net/src/setup.rs` and its test
  `an_epoch_crosses_only_when_both_offer_it` in
  `rs/moq-net/tests/integration/extensions.rs`.
- **JS.** Delete `js/net/src/ietf/epoch.ts` and its tests, the `epoch`
  option in `js/net/src/connection/extensions.ts`, the handshake's
  `Ietf.Epoch.fromSetup`, the `Ietf.Epoch` export, and the EPOCH paths in the
  IETF publisher, subscriber, and message codecs.
- **Tests.** The integration tests that run epoch scenarios across versions
  (`replacement`, `rejoin`, `route_change`, `catalog_handover`, `multi_cdn`,
  and the rest under `rs/moq-net/tests/integration`), and the interop clients
  in `test/interop/clients`, expect epochless behavior on moq-transport
  instead of epoch behavior. Delete the cases that only pinned EPOCH framing.
  [A demoted subscriber priority reaches the publisher](/quest/m1/ts-reliability/stitch-catch-up.md)
  has a "moq-transport-18 after an epoch switch, relayed" case. Whichever
  lands second turns that case into an epochless restart. If a compat cell
  in `test/interop/compat.sh` asserted an epoch across moq-transport, add a
  planned break to `test/interop/compat/planned-breaks.json`.
- **Docs.** Say moq-lite 07 only where a doc promises epochs or redundant
  ingest over moq-transport: `doc/setup/redundancy.md`,
  `doc/concept/moq-lite.md`, `doc/concept/standard.md`,
  `doc/concept/stats.md`, and `doc/bin/relay/cluster.md`. Remove the
  `epoch` lines from `doc/bin/relay/config.md`. The PR's Impact section says
  how to migrate (delete any `epoch` extension setting), for the release cut
  to write into `MIGRATE.md`.

Run `just check`, `just test interop --all`, and the compat lane.

Public API: breaking. `setup::Extensions::epoch` and JS `extensions.epoch`
are removed, and so is `Ietf.Epoch`. Wire: moq-transport sessions stop
offering EPOCH and never send its parameter. moq-lite is unchanged.

## Related

- [Announcement takeover](/quest/m1/announce-takeover.md) - the lite-07 takeover, which no moq-transport counterpart follows
- [Our moq-transport publishers serve at most one group](/quest/m1/ietf-publisher-one-group.md) - the same narrowing for FETCH
