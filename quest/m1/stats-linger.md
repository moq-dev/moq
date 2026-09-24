# [XS] Stats linger

## Goal

At `--stats-depth` above 0, a group broadcast stays announced for one minute
after its last row. A project whose sessions come and go on a node then no
longer ends and restarts a cluster-wide announce on every gap. A lingering
group emits nothing, so aggregate consumers read the same counters.

## Plan

`publish` in `rs/moq-stats/src/produce.rs` drops a group on the first drain
with no traffic or session rows. Record the tick a group went idle, reset it
when rows return, and drop the group only after one minute of idle ticks.
Keep the linger a constant; depth 0 is unchanged.

Test it tick-driven: a session that closes leaves its group announced through
the linger, a session returning within it keeps the same announcement with no
end or start, and the group is withdrawn once the linger passes.

## Related

- [Tree-routed announcements](/quest/m1/announce-tree/README.md) - cuts each
  announce's fanout, where this cuts the announces
