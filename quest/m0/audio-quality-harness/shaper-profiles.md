# [M] The audio quality matrix grades a bursty path and a mid-run step

## Goal

`just test audio-quality` gains the two profiles `moq-shaper` cannot express
today: `bursty`, which releases datagrams in batches (the flush-span shape from
#3477, seven datagrams per 160 ms window), and `step`, which changes the
profile part-way through a row and forces the adaptive target to move. Both get
budgets in `test/audio-quality/budgets.json` and run nightly with the rest.

## Plan

The browser lane landed on the shaper as it is: one uniform profile both ways,
fixed for the life of the process. Its `mild` and `wide` profiles spread
arrivals with uniform jitter, which also reorders datagrams whenever the
jitter exceeds their spacing, so `wide` measures reordering as much as spread.

The reporter's fork (`fperex/moq`, branch `debug-findings-solution`,
`rs/moq-shaper`) already has what is missing, as separate commits: batches that
release datagrams together, steps that change a profile mid-run, an
order-preserving gaussian jitter model, named profiles loaded from TOML, and a
JSON report of the counters. Upstream the batch, step, and order-preserving
jitter changes with the fork author's credit, keeping each behind the options
the drills do not set so their lane is unchanged. Skip TCP passthrough: the
harness answers the page's certificate fetch itself.

Then add `bursty` and `step` to `run.sh`, refuse a `--duration` that ends
before the step, and switch `mild` and `wide` to the order-preserving jitter so
their names mean spread alone. Re-record the budgets for every row whose profile
changed.

## Related

- [Audio jitter target](/quest/m0/audio-jitter-target/README.md) - the estimator the step profile exercises
