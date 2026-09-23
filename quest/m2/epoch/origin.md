# [L] Origin: mint, follow, and resolve epochs

## Goal

In `moq-net` (Rust and JS), publishing a broadcast at a bare path announces it
under a fresh epoch. A consumer of a bare name follows the newest epoch with
a live route and falls back to an older live one. A bare request resolves the
same way on every protocol version, so the relay serves old clients without
any wire change.

## Plan

- Publish: the broadcast create-and-announce path appends `Epoch::mint()`
  unless the path already has an epoch. `announce(prefix, route)` stays raw.
- Consume: `routed` and `request_broadcast` on a bare name watch the routes
  one `@` segment below it. They pick the greatest live epoch, and on a table
  change, re-select: move up at once, or fall back when the current one is
  retracted. A path that names an epoch pins it and never moves. Each move is
  a new broadcast to the caller, never a splice. The #3312 first-hop resume
  rule still applies within one epoch path.
- Bare resolution: a bare request with no route of its own resolves to that
  epoch. A takeover ends the bare subscription with a typed reset, never a
  silent switch. Choose the code so existing clients resubscribe rather than
  give up.
- A nested epoch (`name/@e/derived/@f`) resolves per level. Split-horizon
  exclusion and the per-path fronts from #3312 stay intact.
- Benchmark resolution swept over epochs per name and names per origin, so
  following does not scan the table.
- Update `doc/concept` and `drafts/draft-lcurley-moq-lite.md` wherever they
  describe resolution or takeover. The rule is a relay behavior, so state it
  in the draft even though no field changes.

Public API: behavior change on publish (the announced path gains an epoch)
and on bare-name consume. Decide at PR time whether that retargets to `dev`.
Wire: none.

## Related

- [#2991](/quest/m1/2991-net-coalesce-dynamic-tracks-and-preserve-sequences-across.md) - a new epoch starts each track at sequence 0
- [Broadcast route](/quest/m1/js-broadcast-route.md) - the JS claim matching the follow logic sits on
