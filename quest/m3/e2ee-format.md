# [L] moq-e2ee publishes its finished format

## Goal

`moq-e2ee` and its JS twin implement one finished format, profile
`moq-e2ee-01`, and the crate publishes again. In that format:

- A key binds the broadcast it protects, so a relay cannot answer a request
  for one broadcast with another broadcast under the same credential.
- Each group has its own key, so no broadcast ever has to restart because a
  key ran out.
- An application can encrypt a single track it already holds under a name
  both ends agree on, without the broadcast wrapper choosing the name.
- A publisher is identified by the moq-net `Instance` it announces, minted
  or the application's own stable one; there is no separate `Epoch` type,
  and a restart under a stable Instance never reuses a key.
- The wrapper's names and signatures match moq-net's, in Rust and JS.
- The payload is ciphertext plus tag with no compression metadata, and a
  credential names its profile.

Encryption stays inside the payload: a relay sees nothing that says a track
is encrypted, and moq-net and the wire carry nothing for it.

## Plan

Decided 2026-11-16 (maintainer, /quest-plan of the pre-cut audit):

- ✅ Bind the semantic broadcast name into key derivation, the same string
  the application passes to `credential.path(semantic)` and never the wire
  path (rejected: dropping the argument so `context` alone names the
  broadcast; documenting that `context` must be unique per broadcast). The
  wire path is not stable: a relay may serve one broadcast under a prefix or
  a vanity host, so two ends can disagree on it while agreeing on the name.
- ✅ One key per group; `Exhausted` and the budget accounting are deleted
  (rejected: an in-payload re-key signal; documenting the restart).
- ✅ The per-track wrap is public (rejected: adding it when someone asks;
  dropping the broadcast wrappers and name hiding).
- ✅ `e2ee::Epoch` becomes `moq_net::Instance` (rejected: renaming it
  `e2ee::Instance` and keeping UUIDv7; documenting the difference).
- ✅ The producer takes the caller's Instance, or mints one when the caller
  gives none, and the key scope binds no epoch (2026-11-24, maintainer,
  kixel.ai's [git over MoQ](https://github.com/kixel-ai/kixel.ai/blob/main/quest/m2/git/README.md)
  planning; rejected: minting an Instance per generation, which leaves an
  application that names its publisher by a stable Instance unable to
  encrypt, as kixel's git broadcasts do with each device's endpoint ID;
  binding lite-07's epoch too, since readers stitch every route of one
  Instance without seeing its epoch). Per-group keys need nothing more:
  `with_instance` (`rs/moq-net/src/model/origin.rs`) requires that group
  starts never restart within an Instance, so a restarted publisher derives
  only keys it never used.
- ✅ Wrapper fixes: `abort` takes an error, `create_group` is deleted, the
  subscribe shape and names match moq-net, `name()` means one thing, and
  `Config`, `Frame`, `Datagram`, and `Event` are `#[non_exhaustive]`.
- ✅ A datagram consumer's `recv` (`poll_recv` in Rust) yields `Datagram`
  alone, and its rejects (authentication, duplicate, identity, oversize) are
  counted, not returned, so `Event` is deleted in Rust and JS
  (2026-11-18, maintainer, /quest-plan of the enum-return audit). Every
  caller skips the four rejects, which carry only a sequence, and building
  them matches `Error` variants (`datagram.rs`, `group.rs`, js
  `failure.ts`'s `code`); `Key::open` reports what the caller must do, and
  group.rs stops matching on errors too.
- ✅ No compression in the profile, and `Config` gains `profile` (rejected:
  designing the inner header now; shipping and planning a third label).
- ✅ Nothing on the moq-net layer or the wire says a payload is encrypted or
  how. The maintainer weighed a track property that would let a relay parse
  the scheme and dropped it: a relay that strips the tag to transcode has
  defeated the purpose.
- ✅ The finished format is `moq-e2ee-01`, and the crate stays unpublished
  until it is (see [the crate move](https://github.com/moq-dev/moq/pull/8947)).

Facts found by the audit (paths as of 2026-11-16, under `rs/moq-e2ee/src`
since the crate move):

- Broadcast swap. `scope` is `context || epoch || kid`
  (`generation.rs:171-176`), and the semantic broadcast name enters only the
  path derivation (`credential.rs:108-115`). `credential.consume(epoch)`
  takes no broadcast (`credential.rs:104`). A room that shares one
  credential and publishes two broadcasts has two paths and two epochs; a
  relay that answers a request for the first path with the second's
  Instance and tracks is accepted, because the subscriber derives names and
  keys from the epoch the relay gave it. The draft's "A relay that presents
  a wrong epoch causes authentication failure" is false when the wrong epoch
  is another broadcast's real one.
- Key limits. One AES key per track per epoch, capped at 2^36 plaintext
  bytes and 2^24 operations (`limits.rs:35,38`). 2^36 bytes is 19.1 hours at
  8 Mb/s; 2^24 frames is 4.7 hours for a 1 kHz datagram track. After that
  every write is `Error::Exhausted` (`key.rs:76,80`) and the only remedy is
  a new generation: a new Instance and new names for every track. Failed
  opens count, so a relay can spend a subscriber's budget. The consuming
  side's claim maps are never evicted (`generation.rs:31-35`).
- Per-track entry. `generation::Consumer::{consume, consume_datagrams}` and
  `generation::Producer::{produce, produce_datagrams}` exist and are
  `pub(crate)` (`generation.rs:111,120,231,248`). Keys bind the semantic
  name, not the physical one (`generation.rs:146-153`), so a track under a
  plain wire name seals and opens with the same derivation. Kixel's one
  recorded wish for encryption is a single control track.
- `Epoch` is a UUIDv7 announced as the route's Instance (`epoch.rs:5-12`),
  while lite-07's Epoch is a counter inside one Instance and
  `moq_stats::Epoch` is a bucket's run. Nothing outside tests compares
  epochs, so UUIDv7 ordering has no reader; it costs a type, an error,
  `epoch.json`, and the `uuid` dependency in both languages. A publisher
  that forgets `with_instance` gets no error and no subscriber finds a key.
- Wrapper drift. `track::Producer::abort`, `group::Producer::abort`, and
  the datagram producer's take no error (`track.rs:83`, `group.rs:109`,
  `datagram.rs:148`), while `broadcast::Producer::abort` does
  (`broadcast.rs:300`). `create_group(start)` (`track.rs:57`) cannot go back
  as moq-net's can, since a claim refuses a start at or below the highest
  allocated. `e2ee::track::Consumer` (`track.rs:211`) is what moq-net calls
  `track::Subscriber`. `name()` is the physical name on the track and
  datagram halves (`track.rs:30,229`, `datagram.rs:75,240`) and the semantic
  name on both `Request` types (`track.rs:130`, `datagram.rs:196`). Only
  `Epoch` and `Error` are `#[non_exhaustive]`. Rust and JS already disagree
  (`append_group(start)` against `appendGroup()`, group `close()` against
  `finish()`, an `Error` enum against `Failure` with a `code`).
- Compression. The draft says "There is no inner header"
  (`drafts/draft-lcurley-moq-e2ee.md:266`) and also that a compressed track
  compresses before AEAD (`:286`), and
  [the trait quest](/quest/m5/net-wrap/e2ee.md) planned an authenticated
  flag and inflated size inside each sealed object. Compressing before
  encrypting leaks length when one frame mixes a secret with input another
  party chooses. `Config` has no profile field (`credential.rs:18-25`),
  though the draft says the profile is named beside the credential and a
  mismatch MUST be refused (`:88`); that is how `main` came to carry a second
  derivation under the published `moq-e2ee-00` label.

For the PR (one PR, since every item changes the derivation or the same
types, and the vectors regenerate once):

- Broadcast binding: recommended as
  `scope = context || broadcast || instance || kid`, with
  `credential.produce(semantic)` and `credential.consume(semantic, instance)`
  taking the name, so `path(semantic)` and the generation cannot disagree.
  Add a test that a subscriber for one broadcast refuses another broadcast's
  Instance and tracks under the same credential, and correct the draft's
  sentence.
- Per-group keys: add the group's wire ID (its start, after
  [lite-07 addresses groups by start](https://github.com/moq-dev/moq/pull/8908))
  to `key_info`. For datagram tracks recommend the sequence's high bits as
  the key index, so one HKDF-Expand covers a run of datagrams; settle the
  split in the PR and bench it with `benches/protect.rs`. Then delete
  `Exhausted`, both counters, failed-open accounting, and the consuming
  claim maps. The producing claims that stop a nonce repeating stay.
- Per-track wrap: make the four `generation` methods public, taking any
  moq-net track and the semantic name. Raw seal and open with caller-chosen
  identities stay private, as the epic decided: the claim is what makes
  nonce reuse unrepresentable.
- Instance: `credential.produce(..)` takes an optional `moq_net::Instance`
  and mints one when given none, `generation.instance()` returns it, and
  `consume` takes one. Confirm that a datagram track's key index cannot
  repeat within an Instance across restarts, as group starts cannot; if it
  can, stop and report, since no epoch reaches the subscriber to separate
  runs. Delete
  `Epoch`, `epoch.json`, and `uuid`. The wrapper cannot see whether the
  route was announced with that Instance (a producer's
  `broadcast::Info.instance` is `None`), so say in the docs what a
  subscriber sees when it was not, or have the broadcast wrapper announce
  the route itself.
- Wrapper: `abort(self, Error)` everywhere; delete `create_group`;
  `broadcast::Consumer::track(name)` then `.subscribe(..)` in place of
  `subscribe`; rename `track::Consumer` to `track::Subscriber`; `name()` is
  the semantic name everywhere, with the physical one under another name;
  `Frame.plaintext` becomes `payload`. JS takes the same names in the same
  PR. Error conversion into `moq_net::Error` stays
  with [the trait quest](/quest/m5/net-wrap/e2ee.md).
- Profile: `Config.profile` is a `#[non_exhaustive]` enum with one variant,
  and a credential naming another profile is refused. Delete the draft's
  compression rule and the Catalogs section's `.z` sibling rule; an
  application compresses its own plaintext.
- Label: drop the `-wip` suffix from `moq-e2ee-01-wip` everywhere
  (constant, salt, labels, the vector file's name and its `profile` field,
  `drafts/moq-e2ee-01-wip.ts`, the draft's Profile Version section, both
  READMEs, [the epic](/quest/m5/e2ee/README.md)). While in the draft: it
  still says "the group's sequence" for the nonce, `invalid_secret` has no
  Rust form, and the datagram allowance is 28 bytes, not the epic's 24.
- Publish: remove `publish = false` and the READMEs' unpublished notes,
  give `rs/moq-e2ee` a `MIGRATE.md` for users of crates.io's 0.0.25 (the
  entries `rs/moq-crypto/MIGRATE.md` held before the crate move, in
  `git show 6488ce6b87:rs/moq-crypto/MIGRATE.md`, with their paths back to
  `moq_e2ee::`, plus this format's breaks), and restore the doc rows
  [the crate move](https://github.com/moq-dev/moq/pull/8947) dropped, still marked
  experimental. Submitting the draft's -01 revision to the datatracker is
  the maintainer's, below.

Public API: `moq-e2ee` and its JS twin, both unpublished until this lands.
Wire: the E2EE payload format changes from the published `moq-e2ee-00` to
`moq-e2ee-01`; the two do not interoperate and say so by profile. moq-net
and moq-lite: none.

## Waiting

- Maintainer: submit `draft-lcurley-moq-e2ee-01` to the datatracker once the PR merges, and add a crates.io trusted publisher for `moq-e2ee` (repository `moq-dev/moq`, workflow `release-rs.yml`, environment `publish`); both need a login an agent does not hold. Done when the datatracker shows -01 and a `release-rs.yml` run publishes `moq-e2ee` through OIDC.

## Related

- [End-to-end encrypted MoQ](/quest/m5/e2ee/README.md) - the questline that builds publishers and players on this format
- [moq-e2ee implements the moq-net traits](/quest/m5/net-wrap/e2ee.md) - the later reshape onto moq-net's traits, which this shrinks
