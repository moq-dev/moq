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

- [Closing a binding broadcast ends its media](/quest/m1/broadcast-lifecycle/ffi-close.md) - moq-ffi and moq-c media handles end with a cause on close or drop, never a cause-less drop
- [lite-03/04 close FIN](/quest/m1/broadcast-lifecycle/lite-close-fin.md) - a graceful relay close delivers pending SUBSCRIBE FINs on lite-03 and lite-04
- [Timer cleanups](/quest/m1/broadcast-lifecycle/timers.md) - `ConnectionProps.linger` is gone and stale linger references are fixed

## Related

- [SRT on the shared socket](/quest/m2/one-port/srt-demux.md) - an SRT ingest whose peer goes silent aborts instead of finishing as if it had shut down

- [moq-transport finish interop](/quest/m2/ietf-finish.md) - the interop `finish` case, whose late readers rely on retention
- [CLI and gst flags](https://github.com/moq-dev/moq/pull/6522) - removed the opt-in `linger` booleans, leaving completion to this epic
- [Publication owner](/quest/m1/api-audit/announce-owner.md) - adds `origin::Publication`; it must not forward a `finish` this epic deletes
