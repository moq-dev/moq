# [M] The export clock keeps only the rules admission still needs

## Goal

The TS, MKV, and FLV exports' jitter buffer is bounded by admission alone
(time once a clock exists, bytes throughout), and the export clock sheds
each rule about which frames count that this invariant made redundant.
Land what the measurement supports; record what it does not.

## Plan

About a dozen fixes to the export clock (f85547ffd0, 5d5974f083,
3fdf46160b, e84ffb3de3, a1465d0d00, 11bfa30461, 3cf978cdbf, 493d8eee99,
7a3890cd30, 3b77d23015, 7c4c48d08b, 41f3087e3c) each added a rule about
which frames count. With admission as the single invariant, check whether
acquisition, the leapt/lagging arms, and some of the nine tuning constants
in `rs/moq-mux/src/jitter.rs` can go.

Landed in [moq-dev/moq#7012](https://github.com/moq-dev/moq/pull/7012):

- Admission is time-bounded once a clock exists: a track is full once its
  newest queued frame is due past the delay plus its lead. Leads are seeded
  from acquisition, and a per-track wake reopens a future tail before it is
  due. A 120-second feed with audio at 1x and video at 3x held 4,938 frames
  before and at most 54 after.
- Admission is byte-bounded throughout: a track is full while its queued and
  acquiring frames hold the per-track budget (`MAX_CACHE_BYTES`, as the
  container consumer's default max buffer). Before a
  clock exists, the budget alone bounds acquisition; acquisition still
  anchors on the freshest frame of what it read.
- A backpressured reader still receives group handles (`poll_received`), so
  a finite tail is recognized only after track FIN and every group's FIN, and
  never-decoded evicted handles are reaped by amortized doubling compaction.

A time bound on acquisition (stopping at a delay's worth of backlog) was not
taken: it changes the freshest-frame anchor and the lead of a track sampled
after it, while the byte budget already bounds memory.

Public API: none. Wire: none.

## Related

- [TS fidelity and recovery](/quest/m1/ts-reliability/README.md) - the import side's state ownership
