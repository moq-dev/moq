# [M] MoQ Boy viewers publish their commands again

## Goal

MoQ Boy sends input without socket endpoints, in the format `release` still
runs. Each viewer publishes a broadcast under the viewer prefix with a
`command` track; the emulator follows the announcements under that prefix,
reads each viewer's commands, and reports every viewer's latency in the
shared `status` track. A `release` viewer and a `main` emulator interoperate
again, and neither side needs lite-07 on every hop.

## Plan

Decided 2026-11-16 (maintainer, /quest-plan of the pre-cut audit): ✅ go back
to the old wire format (rejected: keeping the demo on sockets, which would
have to depend on Kixel once [sockets leave](/quest/m2/sockets/README.md)).

The old format, as `origin/release` and
`ed8fe966538c^` (the parent of
[moq-dev/moq#8008](https://github.com/moq-dev/moq/pull/8008)) have it:

- **Viewer** (`js/moq-boy/src/game.ts` then, `demo/boy/src/game.ts` now):
  on first input it opens a route at
  `{prefix-viewer}/{game}/{viewer id}`, a random six-character id, and
  creates a `command` track with a millisecond timescale. Each command is a
  `moq-json` snapshot update: `{"type":"buttons","buttons":[..],
  "timestamps":[{"label","ts"}]}` or `{"type":"reset"}`. The route closes
  after 60 s without input.
- **Emulator** (`rs/moq-boy/src/input.rs`): follows announcements under
  `{prefix-viewer}/{game}/`, reads each broadcast's `command` track through
  `moq_json` so snapshots and merge-patch deltas rebuild a whole command,
  and treats a broadcast going offline as that viewer leaving.
- **Status** (`rs/moq-boy/src/status.rs`): `latency` is a map from viewer id
  to that viewer's ordered latency components, on the shared `status` track.
  The player count is the number of keys.
- `--prefix-viewer` (default `{prefix}/viewer`) and the element's
  `prefix-viewer` attribute name the viewer prefix. The relay lets anonymous
  viewers publish under it (`demo/viewer/**` on moq.dev).

What #8008 changed and this undoes: the socket `Endpoint` on both sides,
`players` in place of `latency` on `status`, `Game.latency` in place of
`Game.viewerId`, `GameConfig.endpoint`, the `viewer-*` name refusal, the
default offer of only `moq-lite-07-wip`
(`rs/moq-boy/src/main.rs:127`, `demo/boy/src/element.ts:21`), and the
refusal of `--prefix-viewer` through `Deprecated::removed`.

Restore the format, not the commit: the tree moved since. For the PR:

- The element lives in the private `demo/boy` now
  ([moq-dev/moq#8446](https://github.com/moq-dev/moq/pull/8446)), and
  moq-json and the announce listing changed API in between; write against
  today's API and keep today's tests where they still apply.
- Keep the fixes #8008's reviews found that are not about sockets: a command
  group lost as `Old`, `Evicted`, `TooFarBehind`, or `GroupTooLarge` is
  skipped, not fatal, and the command reader takes a zero delay budget.
- Re-decide rather than restore, each with a recommendation:
  - The anonymous publish grant. Recommended: `demo/viewer/**`, as `release`
    and moq.dev's auth server have it today, so the hosted deploy changes no
    grant. It lets any viewer publish under another's id, as before; the
    emulator trusts nothing in a command beyond a button state.
  - The version list. Recommended: both sides offer their default list
    again, and `demo/relay/localhost.toml` and `prod.toml` drop the comment
    that lite-07 is listed for sockets.
  - `status`. Recommended: `latency` returns and `players` goes, so a
    `release` viewer reads a `main` emulator. Every viewer sees every
    viewer's latency again, which #8008 had fixed; say so in the PR.
- `rs/moq-boy/MIGRATE.md` and `demo/boy`'s notes drop the entries #8008
  added, since nothing released ever saw the socket form.
- Update `doc/bin/demo.md` (the Sockets bullet) and the demo relay configs.

Tests: the emulator's command tests (malformed command skipped, a superseded
command skipped, each viewer broadcast its own viewer, a viewer leaving) and
the element's round trip (input publishes, the idle timer closes the route).
Then run the demo locally and press a button from a browser.

Public API: none published (`moq-boy` is `publish = false` and `demo/boy`
is private). CLI: `--prefix-viewer` returns. Wire (app level): `status`
carries `latency` again.

## Related

- [moq.dev's hosted demos run the cut](/quest/m3/release/demos-deploy.md) - deploys this
- [moq-json writes take a value and a time](/quest/m2/json-write-at.md) - changes the write the `command` track uses
