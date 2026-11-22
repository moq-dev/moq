# [M] A relay serves a time range inside one group to the next hop

## Goal

A lite-07 range whose start and end both fall inside one group's span, such as
`1500..=1999` over groups starting each second, delivers that group to a reader two
relay hops from the origin, as it does direct and through one copy. Today a Rust relay's
wire publisher, serving the range from its own copy, ends it empty: the reader gets
nothing and its read ends.

## Plan

- Reproduce with the range table: drop the `switch` marker on "a time end between two
  groups stops after the group containing it" in `rs/moq-net/tests/range-table.json` and
  run `range_table::route_switch`. The reader is already empty before the switch, so a
  plain chain (origin, relay A, front) shows it.
- Start in `lite/publisher.rs` (`pin`, `resolved_keys`, the range's settled end) and
  where the relay's copy resolves a time end it holds no group past.
- The JavaScript relay passes the same case (`relay-cold` in `just test relay-fill`,
  where its publisher serves a Rust copy from its own), so its range answer is the
  reference to compare against.

Public API: none. Wire: none expected.
