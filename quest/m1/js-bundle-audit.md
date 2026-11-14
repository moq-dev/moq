# [M] Audit what the JS bundles carry

## Goal

Know what each consumer bundle the Nightly size report measures (`net`, `hang`,
`watch`, `watch-ui`, and `publish`, built by `sh/size/bundles.ts`) carries, and
shrink each by what can be split out, lazy-loaded, or dropped without changing
a package's behavior. Then Nightly's 30-day size comparison passes again,
either because the bundles shrank or because a post-growth report became its
anchor.

## Plan

Decided 2026-11-13 (maintainer, /quest-complete, in
[moq-dev/moq#8654](https://github.com/moq-dev/moq/pull/8654)): ✅ accept the JS
bundle growth as feature growth (lite-07 routes, the byte pool, Clock types)
and plan this audit (rejected: accepting it without an audit; holding until
the bundles shrink). The audit is not a gate on the acceptance.

Measured 2026-11-13 on `main` at `a026b71b1`, built as Nightly does, against the
retained reports. After
[moq-dev/moq#8671](https://github.com/moq-dev/moq/pull/8671) dropped
`sorted-btree` and the test-only `Manual` clock from every bundle, brotli is
2% to 3% below the 11-13 report and still 9% to 12% above the 11-07 one:

| Bundle | 10-22 | 11-07 | 11-13 | `main` |
|---|---:|---:|---:|---:|
| `net` | 98,909 | 136,880 | 157,282 | 153,149 |
| `hang` | 119,937 | 157,584 | 178,277 | 174,142 |
| `watch` | 133,560 | 175,294 | 196,114 | 192,662 |
| `watch-ui` | 144,409 | 186,072 | 206,966 | 203,518 |
| `publish` | 131,060 | 169,777 | 189,804 | 186,314 |

Where the 30-day comparison (10%) stands, since the failure is the anchor, not
a new regression:

- It starts failing on 2026-11-21 against the 10-22 report, where `main` is
  1.41x to 1.55x. The anchor then advances to the newest retained report at
  least 30 days old, and every report before 11-07 is at least 10% below
  `main` for every bundle.
- From 12-07 the 11-07 report is the anchor. `net` (1.119x) and `hang`
  (1.105x) still fail by 1.7% and 0.5% of their size; `watch`, `watch-ui`, and
  `publish` pass. This is the stretch the audit can clear.
- From 12-13 the 11-13 report is the anchor, and `main` passes unless it grows
  again.
- Before 12-07 the anchors are the 10-22 to 11-06 reports, which `main`
  exceeds by 13% (11-06) to 55% (10-22) for `net`. No non-breaking shrink
  clears 11-21 to 11-30 (1.47x to 1.55x), so those nights are red by design,
  and [Nightly artifact sizes](/sh/size/README.md) says the comparison is
  neither waived nor reset (a reset deletes every series). Do not read them as
  a new regression, and still read each night's other lines.

Starting points, from the static chunks of the `main` metafiles (the lazy
chunks, such as the IETF stack and the libav polyfill, are already outside
them):

- `net` is about 577 KB raw, 446 KB of it `js/net/dist`: `track.js` 56 KB,
  `lite/subscriber.js` 45 KB, `lite/publisher.js` 30 KB, `origin.js` 30 KB,
  `resume.js` 26 KB, `group.js` 18 KB. Then `pako` 42 KB, `js/qmux` 44 KB, and
  `js/pattern` 14 KB. Does `pako` earn its size against `CompressionStream`?
- `hang`, `watch-ui`, and `publish` carry the same `net`, plus `zod` (37 to
  39 KB, against 9 KB in `net`), so what in `js/hang` pulls the larger part of
  `zod` into every consumer is the first question.
- `watch-ui` adds 123 KB of `js/watch/dist` and `publish` 71 KB of
  `js/publish/dist`; each is a bundle of its own to explain.

Per package, record in the PR: what the bundle carries (the top inputs by
bytes), what can be split or lazy-loaded and what it costs a consumer, and what
the audit changed with before and after byte counts from `bundles.ts`. A change
that needs a breaking release for a published consumer, such as moving an export
behind a subpath, is its own m2 quest; the audit lands the non-breaking ones.
The thresholds stay as they are.

Public API: none unless a finding says so. Wire: none.
