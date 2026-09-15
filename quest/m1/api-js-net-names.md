# [S] @moq/net and @moq/pattern names mirror Rust

## Goal

The browser packages spell every mirrored concept the way Rust does, and
carry no variant methods. Today `ConnectionProps.subscribe` takes an
`Origin.Producer` where moq-ffi says `consume`, `ConnectionProps.linger` and
`ReloadDelay.{initial,max,timeout}` are bare numbers beside the package's own
`Time.Milli`, `readFrameSequence()` is `readFrame()` plus the group and
frame numbers with `readFrame()` implemented by discarding them,
`Origin.normalizeRoute` / `DEFAULT_ROUTE` / `ZERO_COST` stutter inside a
namespace that already exports `Route` and `Cost`, and `@moq/pattern`
throws `PatternError` where Rust has `InvalidPattern`.

## Plan

- `ConnectionProps.subscribe` becomes `consume`
  (`js/net/src/connection/pool.ts`).
- `linger` and the reload delays become `Time.Milli`.
- One `readFrame()` returns the frame with its group and frame numbers;
  `readFrameSequence()` is deleted on `Subscriber` and `Ordered`
  (`js/net/src/track.ts`), decided 2026-09-14.
- `Origin.normalizeRoute`, `DEFAULT_ROUTE`, and `ZERO_COST` move under
  `Route` and `Cost` (`Route.normalize`, `Route.default`, `Cost.zero`) or go
  private if nothing outside `js/net` reads them (`js/net/src/hop.ts`).
- `PatternError` / `ErrorCode` become `InvalidPattern` with
  `InvalidPattern.Code` (`js/pattern/src/index.ts`).

Public API: breaking on @moq/net and @moq/pattern, so on dev. Wire: none.
Update `js/watch`, `js/publish`, `demo/web`, and `doc/lib/js` call sites.

## Related

- [Merge dev](/quest/m1/merge-dev.md) - requires this so the npm packages release under mirrored names
- [Announce names](/quest/m1/api-announce-names.md) - renames `Announce.Event` in the same package
