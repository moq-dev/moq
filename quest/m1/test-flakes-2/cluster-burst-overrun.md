# [S] Cluster burst drill always overruns its bottleneck

## Goal

`bursts_cross_a_cluster::impaired` (`rs/moq-relay/tests/drills.rs`) always
exercises its bottleneck: every run has at least one burst that overruns
the 100 kbit/s link, so its "no burst overran a bottleneck, so none was
exercised" precondition never fails.

## Plan

Found in #5160's loaded runs (2026-10-09): 5 of about 450 runs of the drills
binary at load average 40-60 failed that precondition. Decided 2026-10-09:
keep the precondition, since a run that never overruns tests nothing. Find
why a burst sometimes fits (for example, pacing that lets the shaper drain
between frames under load), then size or schedule the bursts from the
shaper's capacity so the overrun is guaranteed. No retry, and no lowering
of what counts as an overrun.

Public API: none. Wire: none.

## Investigation checkpoint

The impaired drill currently passes on this host with 116/120 groups delivered,
seven FETCHes, and shaper queue overflows. That confirms the recovery path, but
not a deterministic activation guarantee. The shaper's GCRA bucket advances on
`Instant::now()` for each received UDP datagram. Increasing a finite application
burst cannot guarantee queue overflow: the QUIC sender, socket, or shaper task
may be descheduled while the bucket drains. The existing batch option holds
post-shaping departures and therefore cannot freeze ingress accounting.

Preserve the strict queue-overflow assertion. The recommended next implementation
is a one-shot ingress gate in test support: arm after handshake, collect a batch
of actual application-bearing QUIC datagrams within the peer's available credit,
and release that batch into the rate bucket at one instant. Review its ownership,
cancellation, direction, and byte-threshold contract before adding a public shaper
control. Loss must still be drawn per packet, and the chosen batch must exceed the
queue allowance after the seeded loss treatment.

Alternatives are a drill-local transport gate that avoids adding a general shaper
API, or treating activation as probabilistic and weakening the assertion. Prefer
the shaper ingress gate; the latter does not meet this quest's goal. No production
or transport change has been made in this checkpoint.
