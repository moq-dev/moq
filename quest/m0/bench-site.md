# [M] Benchmark regression table

## Goal

<https://git.moq.dev/bench/> is one light, static table that shows
at a glance which nightly benchmarks regressed or improved. Each row is a
series with its latest value, baseline, change, and a small trend line. A
cell is red when the series got worse beyond its own noise and green when it
got better. The page loads fast with every series the nightly records.
Adding sources and measurement modes belongs to
[benchmark sources](/quest/m2/bench-sources.md).

## Plan

Decided with the maintainer on 2026-10-20, replacing the 2026-10-15 choice of
github-action-benchmark's built-in charts. Those charts drew 1,856 Chart.js
canvases on the costs page (1.7 MB of `data.js`), which was laggy, took far
too much space, and hid regressions instead of surfacing them.

- **Format:** generated static HTML. A tiny inline-SVG sparkline per row
  (the last 30 nightly points) is the only graphic, and no chart library
  loads. The action's chart pages are replaced.
- **One page:** costs (lower is better) and rates (higher is better) merge
  into one table. Each row knows its direction, so red always means worse,
  and a throughput drop sorts with a CPU rise.
- **Rows:** flagged regressions, then improvements, open at the top, worst
  first. Everything else sits in collapsed sections per source
  (`bench-rust`, `bench-ffi`, `bench-rust-extra`, `bench-rust-uring`, ...)
  with a text filter.
- **Threshold:** the baseline is the median of the previous 7 nightly points.
  Flag when the change exceeds max(10%, 3 times the series' median absolute
  deviation over those points), both in the series' own unit. A zero
  baseline flags any change beyond 3 times the MAD, which is any change at
  all when the MAD is zero too. A row with fewer points than that shows grey.
  Hosted runners vary between runs, so each series' own spread sets its bar.
- **Stale series:** a failed or cancelled source adds no history point
  (`prepare` in `sh/gh/bench-report.py`), and on 2026-10-20 `bench-rust`
  failed. A series whose newest point is older than the newest nightly
  commit is marked stale with its age, never shown or flagged as tonight's
  result, and a whole source missing tonight shows as one stale section.
  When every source fails, `prepare` raises today and Nightly never
  regenerates the page; it must still rebuild the table from the existing
  history, with every section stale.
- **One point per commit:** re-running the Benchmark site job appends another
  point for the same commit. On 2026-10-20, two re-runs left three identical
  points for `f13283b30`. Collapse points by commit (the latest wins), so
  re-runs and an unchanged `main` don't shrink the baseline.
- **Storage:** github-action-benchmark keeps writing the history
  (`bench/costs/data.js`, `bench/rates/data.js` on `gh-pages`), with its
  alerting off. The table generator reads that history in
  `sh/gh/bench-report.py`'s `index` step, which already writes the landing
  pages. Drop the action only if its format gets in the way, and keep the
  history that's already there.
- The Pages root lists every report directory by its `<title>`
  ([moq-dev/moq#6079](https://github.com/moq-dev/moq/pull/6079)), so the
  table's `<title>` is its label there.

Pages was enabled and first deployed on 2026-10-20, and the site's custom
domain is `git.moq.dev` (chosen by the maintainer on 2026-10-20; the old
`moq-dev.github.io/moq` URLs redirect there). Re-running the Benchmark
site job in place fails with two `github-pages` artifacts, so delete the
stale one first or wait for the next Nightly. Verify the table against at
least two real nightly points, and test the generator's flagging with
synthetic history (a step, a noisy series, a short series).

Document the page, what red and green mean, and local reproduction in
`bench/README.md`.

Public API: none. Wire: none.
