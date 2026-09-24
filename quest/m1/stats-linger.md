# [XS] Stats linger

## Goal

At `--stats-depth` above 0, a group broadcast stays announced for one minute
after its last row. A project whose sessions come and go on a node then no
longer ends and restarts a cluster-wide announce on every gap. A lingering
group emits nothing, so aggregate consumers read the same counters.

## Plan

`publish` in `rs/moq-stats/src/produce.rs` drops a group on the first drain
with no traffic or session rows. Record the instant a group went idle, from
tokio's clock, reset it when rows return, and drop the group at the first
drain one minute after that. The linger is elapsed time, not ticks, so it holds
at any `--stats-interval` and after a stalled ticker. Keep it a constant;
depth 0 is unchanged.

Test it on paused tokio time, at two intervals: a session that closes leaves its group announced through
the linger, a session returning within it keeps the same announcement with no
end or start, and the group is withdrawn once the linger passes.

## Related

- [Tree-routed announcements](/quest/m1/announce-tree/README.md) - cuts each
  announce's fanout, where this cuts the announces
