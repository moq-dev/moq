# [S] moq-relay's handoff tests hold under load

## Goal

`moq-relay`'s `handoff::` integration tests pass however loaded the machine is.

## Plan

Three of them (`a_killed_relay_s_clients_are_reset_by_the_one_it_replaced`, `a_restart_starts_the_new_relay_beside_the_old`, `a_relay_no_successor_takes_over_from_drains`) failed under a full `just check` on 2026-11-19, each with `no READY reached the service manager: Elapsed` after the test's 10s wall-clock `TIMEOUT`, on a branch that touched no moq-relay code ([moq-dev/moq#9112](https://github.com/moq-dev/moq/pull/9112)).
Run alone, all seven pass in 1.2s.
Reproduce under load first, then make the READY wait follow an event, such as the relay process exiting, instead of a deadline.
