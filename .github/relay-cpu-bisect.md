# Relay CPU bisect

Relay instructions per delivered frame between [cdff3c8f95](https://github.com/moq-dev/moq/commit/cdff3c8f952b1d6bd2fd40c3f64eb0fd53b97e95) ([moq-dev/moq#5926](https://github.com/moq-dev/moq/pull/5926) as merged) and `main` at [c68c931025](https://github.com/moq-dev/moq/commit/c68c931025). Per-cell counts, delivery, loss, host load, and source markers are in [the evidence](relay-cpu-bisect.json). The [first checkpoint](https://github.com/moq-dev/moq/blob/8c62afd3195a3307054120226b54e3309a5dbc1f/.github/relay-cpu-bisect.md) and its [per-cell evidence](https://github.com/moq-dev/moq/blob/8c62afd3195a3307054120226b54e3309a5dbc1f/.github/relay-cpu-bisect.json) are superseded.

## Method

`just bench-profile instructions` counts the relay's user and kernel instructions over a five-second window after seven seconds of warmup, divided by the frames the generator's 500 ms reports interpolate over the same window. Builds use the profiling profile with frame pointers and `moq-relay/io-uring,moq-bench/uring`; the relay runs four workers on cores 4-7 and the generator on cores 0-3. Chat is one publisher, 2,000 viewers, 10 fps, one 300-byte frame per group; video is the `video` workload at 960 delivered frames/s.

One run measures several revisions: every round counts each base and `main` back to back, rotating which goes first. Each revision builds once into its own target and checkout, so later runs reuse it. Every run passed through `quest gate`, and each cell records the host's highest one-minute load during its window.

Three findings shaped the method during this attempt:

- **Host load batches instructions down.** A relay contending with other work handles more frames per wakeup, so each costs fewer instructions: in [run.2DucdO](relay-cpu-bisect.json)'s third round, four revisions read 6-12k lower at load 9-19 than in the two rounds at load 2-5, three read the same, and the two at load 31-56 lost groups. Absolute counts from different sessions are not comparable: the first checkpoint read [55f6ed2292](https://github.com/moq-dev/moq/commit/55f6ed229250d17fc99389237659906d011d8a94) at 142.7k against 169-175k here. Only cells from one run, in a quiet stretch, are compared below. The 24-CPU host's load rose to 20-250 during several runs, from other work.
- **The generator is pinned.** From run.1KRu4S on, `BENCH_GENERATOR=21d6da9fc1` drives every relay, so later client changes cannot move what an old relay spends. With main's generator (run.BIL6LP), five of the seven relays it shares with run.qYPSii's quiet round read within 3k of the pinned figures; [0c2a798de5](https://github.com/moq-dev/moq/commit/0c2a798de5c2e9abb29815b86f945b0a980e5345) and [55f6ed2292](https://github.com/moq-dev/moq/commit/55f6ed229250d17fc99389237659906d011d8a94) read 7k and 11k lower.
- **Counting runs with the sampler off.** From run.qYPSii on, fixed-period stacks (period 2,000,003) come from the five seconds after the counter window, so sampling interrupts stay out of the counts. No measurable bias was found: [55f6ed2292](https://github.com/moq-dev/moq/commit/55f6ed229250d17fc99389237659906d011d8a94) read 167-171k with concurrent sampling and 175k without.

These are sampled-delivery estimates on a shared host. No confidence interval is claimed.

## Chat on io\_uring

The quiet round of run.qYPSii (one-minute load 2-4, every cell at 20,000 frames/s with no loss):

| Revision | kinstr/frame | `learn_end` | `Cursor::probe` | forward drop | syscall leaf |
| --- | ---: | ---: | ---: | ---: | ---: |
| [cdff3c8f95](https://github.com/moq-dev/moq/commit/cdff3c8f952b1d6bd2fd40c3f64eb0fd53b97e95), old endpoint | 117.7 | 0% | 0% | 7.1% | 24.5% |
| [e84ffb3de3](https://github.com/moq-dev/moq/commit/e84ffb3de3139f7245bed5697754fd6add9a022e), before [moq-dev/moq#6224](https://github.com/moq-dev/moq/pull/6224) | 135.7 | 0% | 0% | 7.6% | 21.6% |
| [0c2a798de5](https://github.com/moq-dev/moq/commit/0c2a798de5c2e9abb29815b86f945b0a980e5345) | 169.8 | 15.4% | 0% | 11.7% | 27.5% |
| [55f6ed2292](https://github.com/moq-dev/moq/commit/55f6ed229250d17fc99389237659906d011d8a94), before [moq-dev/moq#6171](https://github.com/moq-dev/moq/pull/6171) | 174.6 | 17.5% | 0% | 11.7% | 31.6% |
| [991572d636](https://github.com/moq-dev/moq/commit/991572d636d264c6189a3477849d2969e689f55c) | 204.7 | 15.5% | 7.7% | 20.1% | 35.0% |
| [2fff7ea7ae](https://github.com/moq-dev/moq/commit/2fff7ea7ae4eff1ac984c1f1af6894e3529f1cf3) | 214.0 | 17.0% | 7.6% | 24.0% | 32.7% |
| [21d6da9fc1](https://github.com/moq-dev/moq/commit/21d6da9fc108b48373a961f34e43c3fcd38b0146), first checkpoint's control | 176.2 | 1.3% | 7.3% | 21.2% | 22.3% |
| `main` | 148.8 | 0.5% | 6.6% | 14.4% | 17.7% |

Markers are inclusive shares of the fixed-period samples; they overlap and are not additive. The two larger rises account for most of the peak, and `main` has already removed both:

- **[moq-dev/moq#6224](https://github.com/moq-dev/moq/pull/6224), 26k of a +34k step, accidental, fixed.** Its `Reader::learn_end` polled each subscriber's finish through the track lock on every forward-drop pass (`poll_forward_drop` → `Subscriber::poll_drop` → `Reader::poll_serving` → `sync` → `learn_end`). It takes 10-18% of all relay instructions from [fd59eb18a5](https://github.com/moq-dev/moq/commit/fd59eb18a50f6e0ac819e73d941a3832cbdac91f), the PR's merge, through [2fff7ea7ae](https://github.com/moq-dev/moq/commit/2fff7ea7ae4eff1ac984c1f1af6894e3529f1cf3), and none at its parent ([run.16AJUb](relay-cpu-bisect.json) has every point in the interval). [moq-dev/moq#7532](https://github.com/moq-dev/moq/pull/7532) brought it to 0.5% on `main`.
- **[moq-dev/moq#6171](https://github.com/moq-dev/moq/pull/6171) and the changes after it, +39k to [2fff7ea7ae](https://github.com/moq-dev/moq/commit/2fff7ea7ae4eff1ac984c1f1af6894e3529f1cf3), mostly fixed.** Dynamic tracks added `Cursor::probe`, 15.8k at [991572d636](https://github.com/moq-dev/moq/commit/991572d636d264c6189a3477849d2969e689f55c) and 9.8k on `main` (below). The rest is track lock contention: futex syscalls and the forward-drop path. [moq-dev/moq#7493](https://github.com/moq-dev/moq/pull/7493) took the fanout subscribers off the shared lock, and `main`'s syscall leaf is now below the old endpoint's (26.3k against 28.8k).
- **[cdff3c8f95](https://github.com/moq-dev/moq/commit/cdff3c8f952b1d6bd2fd40c3f64eb0fd53b97e95) to [e84ffb3de3](https://github.com/moq-dev/moq/commit/e84ffb3de3139f7245bed5697754fd6add9a022e), +18k, a staircase of feature steps.** The first checkpoint measured [moq-dev/moq#5929](https://github.com/moq-dev/moq/pull/5929)'s per-reader ranges and time anchors at +6k. Runs eVjz2w and 2DucdO (main's generator, load 2-5 in the rounds used) put about +6k across [moq-dev/moq#5928](https://github.com/moq-dev/moq/pull/5928) (compressed tracks) and [moq-dev/moq#6129](https://github.com/moq-dev/moq/pull/6129) (a subscriber reads only its ranges' frames), and +12k in steps of 2-4k from [2d73eea67d](https://github.com/moq-dev/moq/commit/2d73eea67d083ecf18938f4fc7c4842118eb87cf): qmux in tree ([moq-dev/moq#6231](https://github.com/moq-dev/moq/pull/6231)), signed priorities ([moq-dev/moq#6142](https://github.com/moq-dev/moq/pull/6142)), the m0 simplification audit and public ingest ([moq-dev/moq#6108](https://github.com/moq-dev/moq/pull/6108), [moq-dev/moq#6111](https://github.com/moq-dev/moq/pull/6111)), and receive leases ([moq-dev/moq#5878](https://github.com/moq-dev/moq/pull/5878)). Each step is within twice the spread between rounds, so these name the interval, not a single commit's cost.
- **[moq-dev/moq#7530](https://github.com/moq-dev/moq/pull/7530) moved io\_uring submission, not cost.** `udp::send` doubled after [21d6da9fc1](https://github.com/moq-dev/moq/commit/21d6da9fc108b48373a961f34e43c3fcd38b0146) because each turn now submits its queued sends inside `Shared::push_inner`, while total syscall instructions fell from 39.2k to 26.3k per frame.

## Main against the old endpoint

Every runtime and workload, with the pinned generator:

| Workload, runtime | Run | [cdff3c8f95](https://github.com/moq-dev/moq/commit/cdff3c8f952b1d6bd2fd40c3f64eb0fd53b97e95) | [21d6da9fc1](https://github.com/moq-dev/moq/commit/21d6da9fc108b48373a961f34e43c3fcd38b0146) | `main` | `main` / old | Highest load |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| chat, io\_uring | qYPSii, quiet round | 117.7 | 176.2 | 148.8 | 1.26 | 3 |
| chat, tokio workers | IV47ua, 4 rounds | 131.1 \[123.6-131.5] | 186.1 \[184.3-189.6] | 171.6 \[167.8-172.8] | 1.31 | 23 |
| video, io\_uring | SkAD0k, 4 rounds | 109.7 \[105.6-119.9] | 140.3 \[138.3-146.8] | 124.9 \[120.5-138.3] | 1.14 | 40 |
| video, tokio workers | Z04ENl, 4 rounds | 152.8 \[148.2-163.9] | 167.3 \[163.2-175.3] | 146.8 \[142.9-147.3] | 0.96 | 40 |

Medians of four rounds, min-max in brackets; every cell delivered its workload's full rate with no loss. The video rows ran at load up to 40, so treat their ratios as approximate. Chat rose on both runtimes, so the remaining cost is shared code. By the nearest named function, chat's growth on `main` falls in the same places on io\_uring (run.qYPSii) and the tokio workers (run.IV47ua), in instructions per frame:

| Growth on `main` | io\_uring | tokio | Introduced by |
| --- | ---: | ---: | --- |
| `Cursor::probe` and `Misses::judge` for every peek | 8.6k | 8.7k | [moq-dev/moq#6171](https://github.com/moq-dev/moq/pull/6171), fills for misses |
| `Cursor` `sync`, `poll_drift`, `poll_peek`, `TrackState::live_edge` and `live` | 11.0k | 9.0k | [moq-dev/moq#5929](https://github.com/moq-dev/moq/pull/5929) and its successors, per-reader ranges and time anchors |
| kio `Tasks::poll_each`, `WaiterList::register` growth, `Waiters::release` | 10.7k | 11.5k | kio's task and waiter rework, including [moq-dev/moq#7340](https://github.com/moq-dev/moq/pull/7340) and [moq-dev/moq#7532](https://github.com/moq-dev/moq/pull/7532) |
| `Subscriber` `poll_drop`, `take`, `poll_peek_group` growth | 5.0k | 4.7k | forward drops and resume |
| `SubscribeServe::poll`, `Writer::poll_close`, transport `poll_closed` | 5.6k | 6.6k | lite publisher serving |

The rows are self cost under the named function, including the std and libc leaves below it; they add to more than the net growth because other paths shrank. `Cursor::probe` reads the subscription, takes the track's shared state lock to judge misses, clones the track's fills, and runs the ask and resolve loop on every peek, even for a live reader that took every group as it arrived and never misses one. That work pays for relay backfill of missed groups, but the chat workload never needs it.

## Exact counts by commit

Relay counts move 5-20% with host load, so a step of a few thousand instructions can't be pinned to one commit from them. `just bench-fanout` (see `bench/README.md`) counts the chat fan-out on one pinned thread instead, against a run of zero cycles, so repeated counts agree within a few instructions per group:

- `model`: one finished group a cycle through the track model, which every subscriber takes. It builds against every revision back to [cdff3c8f95](https://github.com/moq-dev/moq/commit/cdff3c8f952b1d6bd2fd40c3f64eb0fd53b97e95).
- `session`: a publisher session into a relay origin and a viewer session out of it per subscriber, over the in-memory transport. It counts publisher, relay, and viewers together, without QUIC, and builds back to [moq-dev/moq#7027](https://github.com/moq-dev/moq/pull/7027)'s `kio::clock` (2026-11-01).

At 512 subscribers, in instructions per delivered group, the model path cost 4,285 at the old endpoint and 13,300 on `main`, 3.1x; its growth is about 9k of the relay's per-frame growth on chat. The features the maintainer asked about, each against its parent (model, built with frame pointers):

| Feature | Parent | Merge | Change |
| --- | ---: | ---: | ---: |
| Per-reader ranges and time anchors, [moq-dev/moq#5929](https://github.com/moq-dev/moq/pull/5929) | 4,411 | 5,313 | +902 |
| Compressed tracks, [moq-dev/moq#5928](https://github.com/moq-dev/moq/pull/5928) | 5,579 | 5,612 | +33 |
| A subscriber reads only its ranges' frames, [moq-dev/moq#6129](https://github.com/moq-dev/moq/pull/6129) | 5,604 | 5,787 | +183 |
| Signed priorities, [moq-dev/moq#6142](https://github.com/moq-dev/moq/pull/6142) | 5,888 | 5,816 | -72 |
| Public ingest, [moq-dev/moq#6111](https://github.com/moq-dev/moq/pull/6111) | 5,816 | 6,016 | +200 |
| The m0 simplification audit, [moq-dev/moq#6108](https://github.com/moq-dev/moq/pull/6108) | 6,017 | 6,017 | 0 |

In-tree qmux ([moq-dev/moq#6231](https://github.com/moq-dev/moq/pull/6231)) and receive leases ([moq-dev/moq#5878](https://github.com/moq-dev/moq/pull/5878)) are QUIC-level, which neither scenario reaches; their relay counts on the quiet test host are below. The model path barely moved before [e84ffb3de3](https://github.com/moq-dev/moq/commit/e84ffb3de3139f7245bed5697754fd6add9a022e) (5,840) and then grew in a few large steps, each against its parent (run.TmGdFy, one build profile throughout):

| Commit | Model | Session |
| --- | ---: | ---: |
| Dynamic tracks serve fills for misses, [moq-dev/moq#6171](https://github.com/moq-dev/moq/pull/6171) | +4,264 | |
| Refill groups evicted before a reader took them, [moq-dev/moq#7339](https://github.com/moq-dev/moq/pull/7339) | +1,154 | |
| Lite-07 frame bounds, timed datagram admission, layered windows, [moq-dev/moq#7542](https://github.com/moq-dev/moq/pull/7542) | +876 | +3,054 |
| Fill linearly in range count and span, [moq-dev/moq#6995](https://github.com/moq-dev/moq/pull/6995) | +558 | +3,290 |
| Close the relay demand gaps, [moq-dev/moq#7052](https://github.com/moq-dev/moq/pull/7052) | +365 | |
| Bounded reads end through routes, [c68c931025](https://github.com/moq-dev/moq/commit/c68c931025) | +275 | +450 |
| An idle track read parks without walking the read path, [3c0dd8a2af](https://github.com/moq-dev/moq/commit/3c0dd8a2af) | +213 | -3,142 |
| Take a reserved group without asking its cursor again, [68fb455bac](https://github.com/moq-dev/moq/commit/68fb455bac) | +204 | -455 |

The session column starts at the 2026-11-01 harness; blank cells predate it. [moq-dev/moq#6995](https://github.com/moq-dev/moq/pull/6995)'s session cost is the probe's walk of the live groups (`TrackState::live`, +4.0k inclusive) on both the reader's cursor and its forward-drop cursor.

## Fixes in this PR

Each is its own commit, counted in one run (run.s6IjbN in the evidence) at 512 subscribers per delivered group, each row on top of the one above:

| Change | Model | Session |
| --- | ---: | ---: |
| `main` | 13,300 | 120,948 |
| A live reader's probe skips the fills while it misses nothing | 10,477 (-2,823) | 114,139 (-6,809) |
| A window with one set of runs reads without growing across two | 10,126 (-351) | 113,133 (-1,007) |
| A kio waiter remembers 16 lists, not 8 | 10,189 (+63) | 108,709 (-4,424) |
| This branch | 10,189 (-23.4%) | 108,709 (-10.1%) |

`Cursor::probe` is the relay's check for groups a reader has missed. Each time a subscriber asks for its next group, the probe looks at what arrived on the track since its last look and decides whether any group the reader still wants has left the cache without the live feed delivering it (evicted, skipped, or from before the feed started), and asks upstream to fill those. A live reader that takes each group as it arrives never has such a gap, yet it paid for the whole check on every group: locking the subscription and the track's shared state, cloning the track's fills, walking the arrival ledger, checking each new group for holes, and running the ask-and-answer loop. Each time the cache aged out an old group, the eviction also landed with every reader, all of which had already taken it, and forced each one to re-run the full check. The fast path judges a group the arrival walk serves without the hole, backfill, and ledger passes, skips the subscription, fills, and answers when nothing is asked or owed, and starts each reader's eviction inbox above what it handed out. It recovers most of [moq-dev/moq#6171](https://github.com/moq-dev/moq/pull/6171)'s and [moq-dev/moq#7339](https://github.com/moq-dev/moq/pull/7339)'s cost.

kio's `WaiterList::register` was the largest self cost in the session path: a relay session's control and serving tasks park on more than the 8 lists a waiter remembered, so every list past the eighth took a duplicate entry on every poll. 16 measured best; 12 and 32 cost 1.1-1.7% more. Its commit message also claims 2.3% on the model, measured with frame pointers forced; in the build measured here the model moves +0.6%, and the change pays off in the session.

On the quiet test host (run.we4umM, one-minute load at most 4.7), chat over io\_uring went from 154.5k \[154.1-155.0] instructions per frame on `main` to 148.9k \[148.3-150.8] with the probe and window changes (-3.6%), four rounds each, every cell at 20,000 frames/s with no loss. The relay's QUIC, crypto, and syscalls are most of its cost, so the model's -23% shows as a few percent there.

## Validation

Every count, delivery estimate, loss figure, and host load cited here is in [the evidence](relay-cpu-bisect.json), keyed by run artifact. Raw captures (`perf.data`, folded stacks, reports) stay in the measuring checkout's ignored `.tmp/bench/`.
