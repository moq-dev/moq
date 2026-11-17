# [M] @moq/net requests are awaitable, and its last odd names match Rust

## Goal

Three `@moq/net` shapes change before the cut freezes them:

- `await origin.request(path)` returns the `Broadcast.Consumer`, or throws
  the refusal, as `origin.request(path).await?` does in Rust. The handle
  keeps `active`, `closed`, and `close()` for reactive callers.
  `js/net/examples/subscribe.ts` and every hand-rolled wait loop use it.
- `Connection.Props.delay`, the reconnect backoff, is named `backoff`.
- `Connection.Session.closed` and `Auth.Credential.closed` are
  `GetPromise<Error | null>`, so a caller can peek them, as on every other
  handle.

## Plan

Decided 2026-11-16 (maintainer, /quest-plan of the pre-cut audit):

- ✅ `Origin.Requesting` is a thenable that settles once (rejected:
  `request()` returning a bare `Promise` while only `follow()` returns a
  handle, which breaks the entry `@moq/watch` shares between the two;
  keeping the handle and only fixing the example). Reactive callers lose
  nothing, and one-shot callers stop writing a loop that never ends.
- ✅ `delay` becomes `backoff` (rejected: keep). Rust, moq-tokio, and
  moq-ffi all say `backoff`, and `delay` already means two other things in
  this package.
- ✅ Both `closed` fields become `GetPromise` (rejected: keep). It is the
  package's own lifecycle rule.

Facts found by the audit, re-checked at `80ca9787e3`:

- `Requesting` (`js/net/src/origin.ts:2022`) holds
  `active: Getter<Broadcast.Consumer | undefined>` and
  `closed: GetPromise<Error | null>`. A refused request settles `closed` and
  leaves `active` undefined: its doc says it "fails `Unroutable` while
  pending once discovery syncs without a route". Rust's `Requesting`
  resolves to the broadcast or the error (`poll_ok`,
  `rs/moq-net/src/model/origin.rs:7191`).
- The package's hello world waits on `active.changed()` alone
  (`js/net/examples/subscribe.ts:12-16`), so it hangs when the broadcast
  does not exist. `grep 'active\.changed()'` over `js`, `demo`, and `test`
  finds 26 waits; 17 files outside tests hold one, and only
  `test/interop/clients/js-native/replace.ts` and `claims.ts` also watch
  `closed`.
- `Once` is already a thenable over a signal
  (`js/signals/src/index.ts:374`), so the pattern exists.
- `Connection.Props.delay` is `js/net/src/connection/pool.ts:74`, typed
  `ReloadDelay` and re-exported as `Connection.Backoff` (`pool.ts:419`).
  Rust: `pub backoff: Backoff` (`rs/moq-net/src/reconnect.rs:174`). The
  other two meanings of `delay`: `WebSocketProps.delay`, the fallback's head
  start, and `Track.Subscription.delay`, the `{min, max}` playout bound.
  No caller outside `js/net` sets the reconnect option.
- `Connection.Session.closed` is a mutable `Promise<Error | null>`
  (`js/net/src/connection/session.ts:99`) and `Auth.Credential.closed` a
  `PromiseLike<Error | null>` (`js/net/src/auth.ts:56`). `js/AGENTS.md`:
  "Terminal state is one `closed: GetPromise<Error | null>`".

For the PR:

- Awaiting resolves once `active` first holds a broadcast and rejects with
  the `Error` that `closed` carries when the request ends first. `closed`
  can also settle `null` while pending, when the caller closes the handle;
  recommended: that rejects too, with one named cancellation error, so an
  `await` never hangs. `Following` stays a plain handle: it never settles.
- Replace every loop the grep above lists with the `await`, starting with
  the example and `test/smoke/clients/js-native/subscribe.ts`. Add a test
  that awaiting a request for a missing broadcast rejects `Unroutable`.
- Rename the option, the `ReloadDelay` type to match its `Backoff` export,
  the "delay cannot be shared" refusal in `pool.ts`, and `DEFAULT_DELAY`.
  Leave the two other `delay` options alone.
- `Session.closed` also becomes `readonly`. Recommended, not decided:
  `Auth.Issued.closed` and the `@internal` `Auth.Request.closed`
  (`auth.ts:78`, `:109`) take the same type in the same PR, since they are
  the only other `PromiseLike` terminal fields.
- Update `js/net/api/*.api.md` and `js/net/MIGRATE.md`, and any doc page
  that shows the wait loop.

Public API: breaking for `@moq/net`: `Connection.Props.delay` is renamed.
`Requesting` gains `then` and the two `closed` fields widen, both
compatible with code that awaits them. Wire: none.

## Related

- [@moq/net addresses groups by start](/quest/m2/time-addressing/js.md) - the other `@moq/net` rename before the cut
