# [M] Skip ProbeRTT after a near-floor RTT

## Goal

`moq-congestion`'s BBR3 skips a due ProbeRTT when the flow was app-limited at
least once since the last min-RTT refresh and its lowest RTT sample since then
is within 12.5% of `min_rtt`. A skip refreshes the min-RTT stamp and keeps its
value. App-limited media no longer cuts its cwnd every 5 s; a flow that holds a
standing queue, and every bulk flow, enters ProbeRTT exactly as today.

Non-goals: ProbeRTT's cwnd target and duration, and the growth-recognition and
precautionary-probe questions the [BBR media study](/quest/m2/quic-bbr-study.md)
keeps.

## Plan

Decided with the maintainer on 2026-10-24, split out of the BBR media study's
natural-drain section:

- **The cost.** ProbeRTT caps cwnd at 0.5 x `bw` x `min_rtt` for 200 ms plus a
  round every `PROBE_RTT_INTERVAL_SEC`. That cap binds when `bw` sits near the
  media rate (a sender that never measured above its source) or a keyframe
  burst exceeds half the BDP, so frames queue at the sender and it must send
  faster afterward to catch up. On a borderline link (5 Mbps media on 6 Mbps)
  that pushes the flow into dropped frames.
- **The predicate is QUICHE BBRv1's kBBR7**
  ([`ShouldExtendMinRttExpiry`](https://quiche.googlesource.com/quiche.git/+/59748eb0effcf74602957379bf5a8ce0f5aec770/quic/core/congestion_control/bbr_sender.cc),
  `kSimilarMinRttThreshold = 1.125`). ProbeRTT only answers whether
  `min_rtt` is still accurate; a sample near the floor answers it for free, so
  the skip follows whether the queue actually drained rather than a proxy.
  The app-limited gate keeps bulk flows unchanged: they drain near the floor
  every ProbeBW_DOWN and would otherwise never enter ProbeRTT, changing how
  competing bulk BBR flows converge. Their comment's argument for the
  tolerance holds here: a `min_rtt` up to 12.5% low is covered by the 2x cwnd
  gain and ProbeBW's up-gain.
- **Rejected:** skipping whenever app-limited (kBBR8), because the flag is set
  at every frame gap, even while keyframe bursts hold a standing queue, so
  `min_rtt` could ratchet up through a larger BDP and cwnd. An app-limited
  fraction cutoff needs an arbitrary constant and misclassifies the borderline
  link, which is cwnd-limited during each I-frame. Crediting a natural
  low-inflight period as a drain is exactly as safe as ProbeRTT but never fires
  for a flow paced at its own estimate, which is the case this fixes.
- **Google context.** BBR2 and BBR3 dropped kBBR7/kBBR8 and made ProbeRTT
  cheaper instead (kBBR6's half-BDP target, which this BBR3 already has); the
  reason is unpublished. The half-BDP cap still binds in the cases above.
- **Always on**, with no `Config` field: no new public API, and if the shaped
  comparison regresses, the PR does not ship.
- **Not blocked** on the [controller trait](/quest/m1/congestion/trait.md);
  the change is local to `update_min_rtt` and `check_probe_rtt` in
  `rs/moq-congestion/src/bbr3/mod.rs`, and whichever lands second resolves the
  conflict.

Implementation guidance:

- Track the lowest RTT and an app-limited bit since the last min-RTT refresh,
  and reset both at every refresh (a skip included), so each interval needs its
  own evidence. A skip must leave `probe_rtt_min_delay` and `min_rtt` values
  alone; only the stamps move.
- An increase in base RTT beyond the tolerance must still reach ProbeRTT and
  update `min_rtt`. Linux had to stop RTT samples equal to `min_rtt` from
  postponing ProbeRTT forever under persistent loss
  ([fix](https://lkml.iu.edu/2011.2/10337.html)); cover the analogous case.
- Reproduce the periodic stall first, on `Pair`'s rate-limited bottleneck
  (`rs/moq-quic/src/tests/util.rs`) with 30 fps frames and keyframes. One lane
  runs the borderline case, a rate above the media average but below keyframe
  bursts, and shows the stall gone. One overflowing lane, a rate below the
  media bitrate behind a deep but finite queue with verified overflow drops,
  shows ProbeRTT still runs and `min_rtt` does not creep. Record frame
  latency, sender backlog, and ProbeRTT entries before and after.
- Unit tests with mocked time in `moq-congestion`: app-limited near the floor
  skips; a standing self-queue enters; a never-app-limited bulk flow enters; a
  base-RTT step up enters and `min_rtt` follows.
- Docs: none; behavior is internal. Fix any `bbr3` comments the change makes
  stale.

Native probing ([moq-dev/moq#6635](https://github.com/moq-dev/moq/pull/6635)) shrinks the problem once it
validates `bw` above the media rate, since the half-BDP cap then binds less,
but a sender with no probe target still stalls, so this is not redundant.

## Related

- [BBR media study](/quest/m2/quic-bbr-study.md) - growth recognition and precautionary probing, on the same media harness
