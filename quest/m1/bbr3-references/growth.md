# [S] BBR3 recognizes bandwidth growth on any ACK

## Goal

Under ACK aggregation, as on Wi-Fi and cellular uplinks, Startup and
ProbeBW_UP end on a real plateau rather than on three low samples that
happened to land on round starts.

## Plan

Decided 2026-11-14 (maintainer, /quest-plan): adopt Linux's check, provided
it keeps every MoQ change for application-limited senders. The maintainer
answered "I don't know", with that constraint, so the pick follows the
recommendation; reopen it in review if that reading is wrong.

`check_full_bw_reached` (`rs/moq-congestion/src/bbr3/mod.rs`) returns unless
`round_start`, as draft-06 and -07 gate it. Linux checks every
non-application-limited ACK for 1.25x growth and resets the baseline on
any, counting plateau rounds only at round boundaries
([tcp_bbr.c:1924-1948](https://github.com/google/bbr/blob/90210de4b779d40496dee0b89081780eeddf2a60/net/ipv4/tcp_bbr.c#L1924-L1948)).
Move only `full_bw_count += 1` and the `app_limited_rounds` bookkeeping
under the round-start gate; application-limited samples still neither reset
nor advance the plateau. Test bursty, aggregated ACKs and a capacity that
rises mid-Startup, alongside the existing app-limited tests.
