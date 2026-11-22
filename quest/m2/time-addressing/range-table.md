# [M] The range table's route switch leaves part of the range to the new route

## Goal

The shared lite-07 range table (`rs/moq-net/tests/range-table.json`, run by
`range_table.rs`, `js/net/src/range-table.test.ts`, and `just test relay-fill`) switches
routes while the reader still waits on part of its range, in Rust and `@moq/net` alike, so
the new route's copy supplies the rest:

- whole groups the old route never delivered, and
- a group the old route delivered only the head of, so the new copy holds only its later
  frames. A reader whose range covers that group on the new copy gets it whole, the relay
  fetching its head, or sees it named in SUBSCRIBE_DROP when upstream can't supply it.

Today the `switch` mode lets the old route deliver every group before the switch, since
the origin writes them all first and nothing stalls the old link, so it proves the resume
neither repeats nor loses a group and ends where it should, but not that the new route
fills what is left.

## Plan

- Stall the old route's link once the reader has its first groups: Rust's mock transport
  holds uni streams (`hold_unis`), and `@moq/net`'s mock holds a side's writes
  (`MockTransport.hold`), which needs a per-stream release to let only some groups through.
- For the later-frames case, write groups of several frames and switch mid-group. Neither
  language's public API reads part of a group (`group::Fetch::frame_start` is
  crate-private), so the resume at (G, N) is the only way a copy comes to hold one.
- Land it after [moq-dev/moq#9276](https://github.com/moq-dev/moq/pull/9276), which
  changes where a switch caps the old route, and so what each copy holds after it.

Public API: none. Wire: none; a divergence this finds is fixed under lite-07-wip before the cut.
