# [M] moq-bench: every README example fails to parse, and cumulative latency percentiles cannot be windowed to steady state

## Goal

Implement and verify the behavior tracked in [#3126](https://github.com/moq-dev/moq/issues/3126)
within the issue's stated scope and boundaries.

## Plan

Rescoped during the 2026-08 grooming: the README half is fixed on dev by the
usage migration. What remains: per-interval latency percentiles or histograms
so the startup ramp can be excluded from steady-state numbers.

### Issue context

Two smaller things that got in the way of running a relay comparison with `moq-bench` (`dev` @ `fc57e0175`).

#### The README's flag names do not exist

`rs/moq-bench/README.md` documents the target flag as `--client-connect` throughout:

```bash
moq-bench --client-connect https://relay.example.com

moq-bench --file rs/moq-bench/config/hd.toml \
  --client-connect https://relay.example.com \
  --connections 500
```

The binary takes `--connect`; `--client-connect` is not accepted. The client TLS flags in the same section are `--connect-tls-*` (e.g. `--connect-tls-insecure`), not `--client-tls-*`. `--file` is also wrong: the config file is a positional argument, so `moq-bench --file foo.toml` fails with `unexpected argument '--file' found`, matching `moq-relay`'s positional `[FILE]`.

Every example invocation in the README currently fails to parse.

#### Latency percentiles are cumulative, so a steady-state window cannot be measured

The JSONL that `--output` writes carries the counters as cumulative and monotonic, which is documented and is exactly right - a consumer diffs successive lines to get rates. But `latency_p50_ms` / `p90` / `p99` / `max` are cumulative too: they summarize the whole run to date, and there is no way to recover the distribution for a window from two lines.

That matters because the intended methodology is to skip the ramp:

> Run the load from one machine and the sampler on the relay's host, then join the two JSONL files on `timestamp_ms` over the same steady-state window. Skip the `--startup` window while connections ramp.

You can do that for every rate in the file, but not for latency: the `--startup` ramp's samples are permanently baked into the percentiles. In practice this shows up as a p99 that is pure ramp artifact - I was seeing `latency_p99_ms` of 589-826 ms on runs whose steady-state p50/p90 were 1-2 ms, purely because a handful of connections' first groups landed while the swarm was still connecting.

Emitting the per-interval histogram (or resettable per-interval percentiles alongside the cumulative ones) would make latency as windowable as the counters already are. `latency_samples` is already per-line, so the shape is half there.

## Closes

- [#3126](https://github.com/moq-dev/moq/issues/3126) - close this issue when the quest finishes
