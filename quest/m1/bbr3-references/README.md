# [S] BBR3 matches its references

## Goal

moq-congestion's BBR3 matches its references wherever it has not
deliberately diverged: each bug a reference comparison confirmed is fixed
with a regression test on the seeded, bufferbloat-shaped simulation, the
controller follows draft-ietf-ccwg-bbr-07, and `rs/moq-quic/README.md`'s
ledger names the reference commits checked and every deliberate divergence.

Out of scope: differences only measurement could settle (when ProbeBW_UP and
DOWN end, minimum loss events per round, the initial window, clamping samples
by receive timestamps, a CE ratio instead of one CE), since the 2026-11-09
deletion audit ([moq-dev/moq#8423](https://github.com/moq-dev/moq/pull/8423))
pruned the study that would measure them; app-limited bandwidth aging, which
the [probe epic](/quest/m3/probe/README.md) owns; and quinn `main` commits,
which the [quinn sync](/quest/m3/vendor-sync/quinn.md) owns.

## Plan

Decided 2026-11-14 (maintainer, /quest-plan): ✅ fix the confirmed bugs and
adopt draft-07 (rejected: bugs only, skipping draft-07's ProbeRTT rework;
also revive the pruned measurement study).

Found by a 2026-11-14 comparison against
[Google Linux BBRv3 `90210de4`](https://github.com/google/bbr/blob/90210de4b779d40496dee0b89081780eeddf2a60/net/ipv4/tcp_bbr.c)
(unchanged since 2025-03), [draft-07](https://www.ietf.org/archive/id/draft-ietf-ccwg-bbr-07.html)
(2026-11-02), and [QUICHE BBR3 `51a950db`](https://github.com/google/quiche/blob/51a950dba2100da7e7977aa97759470507d1ebb8/quiche/quic/core/congestion_control/bbr3_sender.cc).
[quinn#2481](https://github.com/quinn-rs/quinn/pull/2481)'s latest
force-push (`0ebaf7e2`) only rebased; its BBR3 is the `01b8848d` already
synced.

Constraint from the maintainer: no fix here reverts a MoQ change for
application-limited senders (ProbeRTT skip, app-limited bandwidth aging,
skipping app-limited rounds in plateau counting, the bandwidth probe's
headroom). A reference behavior that conflicts with one keeps ours.

This README's own work, after the children: update the BBR3 entries in
`rs/moq-quic/README.md` to name draft-07 and the three reference commits,
and list the divergences recorded only in code comments today (ProbeRTT
skip, app-limited aging, the one-packet send quantum floor, loss state saved
once per recovery episode, Linux's growth check) beside classic ECN. Record the reference heads there too, so each
[quinn sync](/quest/m3/vendor-sync/quinn.md) rechecks them with quinn#2481.

## Related

- [Probe up to a target bitrate](/quest/m3/probe/README.md) - owns app-limited aging and probe headroom
- [Realtime QUIC](/quest/m4/quic-realtime.md) - the bake-off for algorithms other than BBR3
