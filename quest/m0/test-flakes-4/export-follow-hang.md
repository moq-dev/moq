# [M] A TS follow export notices its publisher leaving when the relay loses the route

## Goal

`export::the_same_instance_within_the_follow_continues` (`rs/moq-cli/tests/integration/export.rs`) never fails with "export never exited" (`export.rs:72`): when the relay loses the publisher's route, `moq export ts --follow` sees its tracks fail, plays out its buffer, and carries on into the return or exits.

## Plan

Seen in the 2026-11-22 Nightly and in 1 of 12 local stress runs on `main` (4250579f4a). It did not recur in 45 runs after [moq-dev/moq#9662](https://github.com/moq-dev/moq/pull/9662), which carries the follower on into a return while it drains, but that fix only acts once the export has seen its publisher leave, so the hang is likely rarer rather than gone.

From the failing run's exporter log: after the abort, `moq_net::lite::subscriber` logs `subscribe error ... err=unroutable` for every track and the catalog, but the export logs no `rendition failed`, no `current group evicted`, and no `handoff phase="left"`. So its track readers never ended, `Export::leave` never ran, and the follower waited on the old tracks until the test's timeout. In passing runs the same errors reach the readers (`track failed; ending the reader`) within a few milliseconds.

- Reproduce with `just rs test -p moq-cli --test integration --run-ignored only -E 'test(the_same_instance_within_the_follow_continues)' --stress-count 40`. If it doesn't reproduce after a few hundred runs, delete this quest.
- Find why a subscription the relay refuses with `unroutable` leaves the subscriber's track open (likely in `rs/moq-net`'s lite subscriber or the relay's handling of a replaced route), and fix it there.
