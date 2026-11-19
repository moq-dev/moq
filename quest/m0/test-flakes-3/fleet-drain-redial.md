# [S] A fleet drain's redial delivers the next group under load

## Goal

`moq-tokio` `connection::tests::a_fleet_drain_redials_through_a_fresh_resolve`
passes under a loaded `just check`.

## Plan

Seen once on 2026-11-18 during `just check` for
[moq-dev/moq#9140](https://github.com/moq-dev/moq/pull/9140), which does not
touch moq-tokio or GOAWAY: the subscriber's `recv_group` for `g1` failed with
`Session(GoawayTimeout)` (`rs/moq-tokio/src/connection.rs`, the `g1` wait). It
passed 5 of 5 alone and 40 of 40 under `--stress-count 40 --test-threads 16`,
taking 0.5 to 3 s alone, so it rides on wall-clock time.

Find why the drained session's deadline can end the subscription before the
redial to B carries `g1`, and drive the timers with a manual clock or assert on
the handover event instead of real time, per the epic's rules.
