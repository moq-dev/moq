# [S] @moq/net exports only what something outside it uses

## Goal

`@moq/net` publishes no export that nothing outside the package calls, and
none under a name that hides what it is. The `@moq/net/zod` subpath and the
`zod` peer dependency are gone; the wire-internal `Varint` and `Path`
helpers, `Track.Subscriber.tryRecvGroup`, and `Track.Request` are internal;
`rescale` and `rescaleIn` are reached through `Track`; and
`Broadcast.Consumer`'s subclass hooks are `@internal`. An export Kixel or
moq.pro calls stays.

## Plan

Decided 2026-11-16 (maintainer, /quest-plan of the pre-cut audit):

- ✅ Remove or hide every public item with no consumer, in one sweep per
  area, each item first re-checked for a caller in Kixel and moq.pro
  (rejected: shipping them; removing without the downstream check).
  Adding an export later is free; removing one after the cut is a break.

Before deleting anything, grep Kixel's and moq.pro's current `main` for
each item below and keep what either calls, naming the caller in the PR.

Items, each with the grep that showed no caller, re-run at `80ca9787e3`
over `js` (outside `js/net/src` and `js/net/api`), `demo`, `test`, and
`doc`:

- **`@moq/net/zod`** (`js/net/package.json:15`; `js/net/src/zod.ts`, 19
  lines, one `read(source, schema)`). `grep 'net/zod'` finds only its API
  report, a MIGRATE line, and a comment in `doc/.vitepress/api/router.ts`.
  Its `write` twin was already dropped. It is the only importer of `zod`
  in `js/net/src`, so the peer dependency (`package.json:31`) goes with
  it. Delete `zod.ts`, `zod.test.ts`, `api/zod.api.md`, and the export.
  `@moq/json` is the schema-validated JSON path.
- **`Varint`** (`js/net/src/varint.ts`): `encodeLeadingOnes`,
  `encodeLeadingOnesTo`, `decodeLeadingOnes`, `encodeTo`, `size`, and
  `MAX_U6`, `MAX_U14`, `MAX_U30`, `MAX_U53` have no reference outside the
  package's own `bench/`, which imports `../src/varint.ts` directly.
  `Varint.encode`, `decode`, and `decodeBigInt` are used (`js/hang`,
  `js/loc`) and stay. Mark the rest `@internal`.
- **`Path`** (`js/net/src/path.ts:107`, `:124`, `:57`): `Path.encode` and
  `Path.decode` have no caller, and both validate rather than encode;
  `Path.MAX_PARTS` has none. One doc comment in `js/auth/src/path.ts:24`
  names `Path.decode`; reword it.
- **`Track.Subscriber.tryRecvGroup`** (`js/net/src/track.ts:4700`): only
  `js/net/bench` calls it, and the benches already reach internals through
  `hooks` (`src/internal.ts`). Move it there.
- **`Track.Request`** (`js/net/src/track.ts:470`) is `@public`, but no
  public API returns one: its doc says it is "created internally by the
  broadcast". Hide it.
  [Broadcast requests](/quest/m4/net-broadcast-requests.md) publishes it
  with `Broadcast.Producer.requests()` after the cut, which is additive.
- **`Datagram.rescale` and `Datagram.rescaleIn`**
  (`js/net/src/datagram.ts:13`, from `rescale.ts`) take any
  `{timescale}`, and `@moq/crypto` calls `rescaleIn` for a stream track's
  frame (`js/crypto/src/e2ee/group.ts:67`, `datagram.ts:92`). Rust has
  `track::Info::rescale` and `rescale_in`
  (`rs/moq-net/src/model/track.rs:289`, `:300`). Export them under `Track`
  and drop the `Datagram` export.
- **`Broadcast.Consumer`** publishes `protected constructor(shared?: never)`
  and `protected shareState(): never` (`js/net/src/broadcast.ts:550`,
  `:647`), an in-package subclass hook. Mark both `@internal`;
  [the JS wrappers](/quest/m5/net-wrap/js.md) replace subclassing later.

Recommended in the same PR, from the audit, not individually decided:

- `Group.Producer.isClosed` and `Group.Consumer.isClosed`
  (`js/net/src/group.ts:698`, `:1330`) duplicate
  `closed.peek() !== undefined`, which `js/AGENTS.md` names as the way to
  ask. Two callers outside the package (`js/crypto/src/e2ee/group.ts:91`,
  `js/hang/bench/buffered.ts:179`). Remove them and move the callers.
- `Connection.Transport` includes `"webrtc"`
  (`js/net/src/connection/transport.ts:2`). Nothing produces that label
  outside two tests since peer-to-peer left in
  [moq-dev/moq#8771](https://github.com/moq-dev/moq/pull/8771). A supplied
  transport may still label itself, so drop it only if the Kixel check
  finds no use, with Rust's `Transport::WebRtc`
  ([transport sweep](/quest/m2/unused-api/transport.md)).

Kept on purpose although nothing in the repository calls them:
`peerImplementation`, `setCost`, `Error.RefusedRedirect`,
`Error.ConnectTimeout`, and `Auth.Unsupported`. They mirror Rust or exist
for `instanceof`. `Track.Requests` and `Track.Fill` stay: they mirror
Rust, and the interop clients use them.

Not here: sockets leave `@moq/net` with
[the socket move](/quest/m2/sockets/README.md), and the `NO_MAX_DELAY`
sentinel (`js/net/src/internal.ts`) goes with the Max Age change in
[the lite-07 wire](/quest/m2/time-addressing/lite07.md). The browser media
packages' leftovers were their own sweep.

For the PR: regenerate `js/net/api/index.api.md` and check that every
removed name is gone from it; add each removal to `js/net/MIGRATE.md`;
`just js check` builds the declarations, which catches an `@internal`
type that a public signature still names.

Public API: breaking for `@moq/net`: the `./zod` subpath and the names
above are removed or hidden, and `rescale` moves from `Datagram` to
`Track`. Wire: none.

## Related

- [@moq/net requests are awaitable](/quest/m2/js-net-api.md) - the other `@moq/net` API changes before the cut
- [Broadcast requests](/quest/m4/net-broadcast-requests.md) - republishes `Track.Request` with its accessor
