# [M] moq-net exports with a consumer

## Goal

The moq-net, `@moq/net`, and moq-ffi items the 2026-10-23 audit follow-up
found without a consumer go private, test-only, or away. The first pass,
which trimmed the model exports added since release and renamed
`Track.Location` to `Position`, landed in
[moq-dev/moq#6470](https://github.com/moq-dev/moq/pull/6470).

## Plan

Decided 2026-10-23 (API audit follow-up, maintainer):

- `Epoch::time` and FFI `moq_epoch_time_ms` with its wrappers: only binding
  tests read them. `moq_mint_epoch` stays.
- `announce::Consumer::is_synced` (tests only) goes test-only, and
  `announce::Consumer::synced` goes; `poll_synced` stays for `Follow`.
- `varint::MAX_QUIC`: `pub(crate)`.
- JS `Group.Demand` and `ConnectProps`/`AcceptProps.extensions`: `@internal`.
- Dead code, found by marking every pub item `#[deprecated]` and building
  the workspace. Delete what nothing references or constructs:
  `Error::TooManyParameters`, `DecodeError::{ExpectedData, Missing,
  Unsupported}`, `frame::Buffer::{is_full, filled_mut}`,
  `frame::Consumer::poll_read_all`, `track::Ordered::poll_finished`,
  `Path::next_part`, `stats::Registry::exclude`,
  `stats::Traffic::{is_announced, total_bytes}`, `stats::Tier::is_default`,
  `transport::poll::Buffered::get_mut`, `setup::Token::CAT`; JS
  `Varint.sizeLeadingOnes`, `SetupOptions.{removeBytes, removeVarint}`, and
  the `deliveryTimeout` accessors.
- Test-only plumbing, deleted with its tests moved to what remains: the whole
  `broadcast::Demand` with both `demand()` getters (JS `Broadcast.Demand` is
  already internal), `goaway::Consumer::{peek, recv}`,
  `bandwidth::Producer::{abort, closed, used, unused}`,
  `Version::{from_alpn, is_ietf, names}`, `Server::with_origin`,
  `stats::Handle::{parent, tier}`, and group
  `poll_unused`/`abort_unused` (their `allow(dead_code)` waited on an
  answering API that is now dropped). Test-only reader and writer
  conveniences (`group::Consumer::read_frames`, `frame::Consumer::read_all`,
  and the like) stay: applications are their consumer.

Kept, so the next audit skips them: `transport::memory` stays public and
ungated, since an in-process transport is real API that other crates' tests
use.

Public API: removals and renames in moq-net and `@moq/net`. Wire: none.
