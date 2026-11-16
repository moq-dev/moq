# [M] The relay's memory growth since release is explained before the cut

## Goal

The relay's extra memory per connection on `main` against `release` has a
named cause, and the cut ships with it either fixed or written down as the
price of a feature. The release comparison reports memory beside CPU, so the
next one cannot miss it. Nothing here changes an API or the wire.

## Plan

Decided 2026-11-16 (maintainer, /quest-plan of the pre-cut audit):

- ✅ Profile it and fix or record it before the cut, and the cut waits
  (rejected: writing the number into the report and shipping; leaving it to
  the [performance audit](/quest/m3/audit-perf.md), whose release run comes
  after the cut by its own `Required`). A regression found after 0.22 is
  announced costs a patch release and a correction.
- ✅ The release comparison reports memory (rejected: CPU only, as today).

Facts found by the audit:

- `.github/release-perf.json` is the 2026-11-05 pairing of `release`
  (`dc3478a863`) against `main` (`56d7989c78`), seven alternating rounds.
  The median of `summaries[*].rss_bytes` per workload, relay process:

  | Workload | release | main | Change |
  | --- | --- | --- | --- |
  | `video` | 31.0 MiB | 28.4 MiB | -8% |
  | `fanout` | 35.8 MiB | 35.5 MiB | -1% |
  | `chat` (2,001 connections) | 428.3 MiB | 501.8 MiB | +17% |

  The `chat` ranges never overlap (428.1 to 429.0 against 499.6 to 503.1),
  and the difference is about 37 KiB per connection.
- `.github/release-perf.md`, the report written from that file, never
  mentions RSS, memory, or MiB (`grep -n -i 'rss\|memory\|MiB'` finds
  nothing), so nobody read the number.
- The only earlier memory evidence for the in-tree QUIC stack is
  [moq-dev/moq#5664](https://github.com/moq-dev/moq/pull/5664): 256
  sessions, measured on a host at load 20 to 56. A downstream deployment's
  condition for relying on the stack is that relay memory matches the old
  one.
- The comparison is of whole relays, so the growth may be the QUIC stack
  (per-connection buffers, the 8 MiB socket buffers), moq-net (per-session
  route and announce state), or the allocator. `grep -rni rss quest/` finds
  only benchmark plumbing: no quest owned it.

For the PR:

1. Reproduce on today's `main` and `origin/release` with
   `bench/workloads/chat.toml`, alternating rounds, recording
   `/proc/loadavg`, with the evidence kept outside the checkout. `main` has
   moved about 850 commits since the pairing, so the number comes first.
2. Attribute it with a heap profile of the relay on both sides
   (`moq_tokio::jemalloc` is on-demand heap profiling; the pairing itself
   ran the system allocator). Report bytes per connection by owner:
   transport, session, origin, other.
3. Fix what is accidental in this PR. A cost that pays for a feature stays,
   recorded in `.github/release-perf.md` with the feature and its PR, as
   the performance audit records accepted CPU costs. A fix too large to
   review here is a follow-up quest in m2, and this quest lists it under
   `Required` until it lands.
4. Report RSS per workload in the release comparison. Each run's summary
   already records `rss_bytes` (`bench/summary.py:91`), but
   `bench/compare.py` never compares it, and the Release baseline section
   of `quest/m3/audit-perf.md` never asks for it. It gets the same
   accept-or-fix rule as CPU.

Do not set a threshold: the outcome is a cause and a decision, not a number.
If the cause turns out to be a public default (a buffer size, a window),
changing it is the maintainer's call; bring the profile.

Public API: none. Wire: none. Config: none expected.

## Related

- [Performance audit](/quest/m3/audit-perf.md) - reruns the release pairing after the cut and inherits the memory column
