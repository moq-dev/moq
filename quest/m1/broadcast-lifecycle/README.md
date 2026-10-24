# Broadcasts are online or offline, and retention is app policy

## Goal

A moq-net broadcast has no lifecycle state of its own: it is online while
announced and offline once closed. The app marks the end of its content with
a track FIN (hang: finish the catalog). Keeping a finished broadcast up for
late readers is a fixed-duration app policy in moq-mux, not a deadline moq-net
derives from track max-age. Every other timer called "linger" has one meaning
per name.

## Plan

Decided with the maintainer on 2026-10-23, from an audit of every linger
timer on main at 1582fdc07d:

- `broadcast::Producer::finish` exists only in moq-net. Nothing reaches the
  wire: ANNOUNCE active/ended already says online/offline, and a track FIN
  already says a track's content ended. A BROADCAST wire message was
  considered and rejected; it adds no behavior over those two and would need a
  moq-transport mapping.
- The finished linger was inconsistent and partly accidental:
  - Its duration is the max of track max-ages, which describe how long a
    superseded group stays fresh and never the newest one. The JS publish
    catalog declares `Number.MAX_SAFE_INTEGER` max-age, so every browser
    publisher lingered the full 30s; Rust catalogs declare none.
  - It counted from different events: Rust from the first poll of `wait`,
    JS from the `wait` call, UniFFI from `finish`, and C from the caller's
    own clock.
  - Only Rust ended it when a newer epoch appeared at the path.
- Closing a broadcast already leaves tracks handed out playing to their end,
  and session drain delivers their tail to attached subscribers. Retention
  only serves readers that arrive after the end.
- A retaining publisher ignores a newer instance at its path: if the newer
  one leaves, the old one is announced again, and readers see its finished
  catalog and stop. Simpler than watching for supersession, and acceptable
  under online/offline.
- The other timers stay. Each is a cache or debounce with its own name and
  owner: the Rust idle track copy (30s), discovery (2s), the JS connection
  pool (2s), and the relay's `[stats] linger` (5m). They are not cross-language
  lifecycle guarantees.
- After this epic, user-facing "linger" means only publisher retention
  (`moq` publishing subcommands, moqsink) and the relay's stats setting. The
  export-side wait becomes `--follow`.
- Docs change inline in each quest; no separate lifecycle page.

## Required

- [Publisher retention in moq-mux](/quest/m1/broadcast-lifecycle/retention.md) - moq-cli, the gateways, and moqsink keep a finished broadcast up for a fixed `--linger` (default 30s) through a moq-mux helper instead of moq-net's `Finish`
- [Stateless broadcasts](/quest/m1/broadcast-lifecycle/stateless.md) - `Finish`, its linger, and finished-only supersession are gone from moq-net, `@moq/net`, moq-ffi, moq-c, and every binding
- [TS export follows by cause](/quest/m1/broadcast-lifecycle/ts-follow.md) - `export ts|srt --linger` becomes `--follow <duration>`, a replacement's catalog has no separate deadline, and a track failure is classified by its cause instead of a 1s grace
- [SRT tells a clean end from a failure](/quest/m1/broadcast-lifecycle/srt-end.md) - an SRT ingest whose peer goes silent aborts instead of finishing as if it had shut down
- [Timer cleanups](/quest/m1/broadcast-lifecycle/timers.md) - `ConnectionProps.linger` is gone and stale linger references are fixed

## Related

- [moq-transport finish interop](/quest/m2/ietf-finish.md) - the interop `finish` case, whose late readers rely on retention
- [CLI and gst flags](/quest/m1/api-audit/cli-flags.md) - removes the opt-in `linger` booleans, leaving completion to this epic
- [JS origin publications](/quest/m1/api-audit/js-publication.md) and [binding publications](/quest/m1/api-audit/binding-publication.md) - mirror `origin::Publication`; they must not forward a `finish` this epic deletes
