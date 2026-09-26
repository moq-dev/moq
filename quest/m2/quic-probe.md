# [L] Discover capacity above the media bitrate

## Goal

The selected QUIC backend validates headroom for an encoder increase without
unacceptable frame delay or excess traffic. Prefer already-buffered useful
media; add opt-in redundant probing only where measurements justify it.
Keep current goodput, historical capacity and its age, and the desired
encoder rate distinct. Preserving an old estimate does not discover capacity.

## Plan

Use the corrected BBR release. First compare ordinary pacing with brief
pacing increases using available useful media, bounded by congestion window,
bytes, duration, and the desired encoder step. Never delay ready frames or
manufacture large keyframes to probe. A small burst can be inconclusive;
stop on persistent queue growth or congestion evidence. Do not force Startup
exit or replace capacity with encoder bitrate merely because the source
plateaus.

Where useful traffic cannot validate wanted headroom, compare early
retransmission of unacknowledged, unexpired data against equal-budget padding,
no probing, and ordinary loss recovery. Duplicates can delay useful bytes;
being useful if the original was lost does not make redundancy free. Keep
all bytes in congestion accounting and count unique media delivery once.
Wire-capacity evidence must remain distinct from application goodput.

Implement only the measured winning mechanism in the fork's recovery and
pacing layer. For redundant probes, evaluate the existing IMMEDIATE_ACK
extension so ACK delay does not obscure a short sample. Enable probing only
while an active application requests headroom, within explicit byte/time
budgets. Carry any chosen estimate through the existing transport estimate,
MoQ PROBE, and publisher adaptation; document public API changes and update
consumer docs inline rather than committing a new API shape in this plan.

Measure an illustrative 2-Mbit/s source seeking 4 Mbit/s while the path steps
3 to 12 to 3 Mbit/s. Reuse identical media traces and wire budgets. Include
keyframes, idle restart, stale high estimates, receiver credit limits,
random/burst loss, shallow queues, compressed ACKs, and competing CUBIC/BBR
flows. Set deadline, overhead, and fairness budgets before tuning. Report
validated upward adaptation, downward reaction, estimate age, frame deadlines,
queue delay, unique goodput, and redundant bytes. A successful small burst
is not proof of full path capacity. Persist regressions in CI and broader
network scenarios at least nightly. A measured no-go is a valid outcome;
retain the baseline and record why before exposing an ineffective option.

## Related

- [Natural media drains](/quest/m2/quic-bbr-app-limited.md) - separate ProbeRTT policy experiment
- [FEC experiment](/quest/m2/quic-fec.md) - repetition competes for the redundancy budget
- [GCC egress experiment](/quest/m2/quic-gcc.md) - delay control changes what headroom means
