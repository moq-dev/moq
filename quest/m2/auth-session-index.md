# [S] The auth server's session caps cost O(1) per connect

## Goal

With `--limit-token` or `--limit-remote` set, the auth server's `connect`, `end`, and `revalidate` cost the same at 16,384 live sessions as at 0, instead of scanning every live session under the server's one mutex. The session table keeps per-token and per-remote counts, and ages out sessions whose relay died from a time-ordered queue instead of a full sweep.

## Plan

Found by [moq-dev/moq#7512](https://github.com/moq-dev/moq/pull/7512), which attributed JWT verification cost and runs it nightly: one `connect` plus `end` costs 66.5k instructions (9.1 µs) at 0 live sessions, 147k (18.6 µs) at 1024, and 1.27M (158 µs) at 16,384, about 73 instructions per live session. The cost is `Sessions::sweep` plus the per-token and per-remote count scans in `Sessions::connect` (`rs/moq-auth/src/serve.rs`, about line 375); `Revalidate` pays the sweep too. Its `verify/sessions/{0,1024,16384}` bench is the baseline.

Decided 2026-11-01 (maintainer, follow-up planning after the 2026-11-01 /quest-afk run): m2. This waits on [moq-dev/moq#7211](https://github.com/moq-dev/moq/pull/7211) landing, which rewrites `Sessions`' token bookkeeping (shared request-token grants and attachment leases); indexing the table first would conflict with it, as #7512 noted.

- Keep `HashMap<token, count>` and `HashMap<remote, count>` beside the slots, updated on connect, end, a new token attaching to a known session, and expiry.
- Age slots from a queue ordered by `seen`, refreshed on each sighting (a re-push with a generation, or an ordered map keyed by `(seen, id)`), so a sweep pops only expired entries. Keep today's rule: a session that missed two cadences is dropped.
- Keep the semantics the doc comment on the limits describes, including a known id attaching another credential counting only against the new JWT's cap.
- Measure with #7512's `verify/sessions` bench before and after; the slope should be flat.

Public API: none. Wire: none.
