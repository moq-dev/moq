# [XS] Tests queued for an mbx permit time out under load

## Goal

No test hits nextest's timeout while it waits for an mbx scheduler permit, so `moq-srt`'s `server::tests` (and every other test) pass however loaded the host is.

## Plan

Four `moq-srt` `server::tests` hit nextest's 60s timeout in a loaded `just check` ([moq-dev/moq#9123](https://github.com/moq-dev/moq/pull/9123)); three are purely synchronous, so the wait was never in the test code.
The cause is mbx, the Cargo wrapper: the host's global mbx config sets `scheduler.tests = true`, so each test waits for a host-wide permit inside `mbx-test-runner`, and nextest counts that wait toward the test's `slow-timeout`.
Any test in the workspace can hit it; moq-srt's were just in flight.

Decided 2026-11-19 (maintainer, [moq-dev/moq#9163](https://github.com/moq-dev/moq/pull/9163#issuecomment-7020242924)): fix it in mbx or the dotfiles mbx config, not with a checked-in `.mbx.toml` opt-out or a longer `terminate-after` here, so host-wide test scheduling stays on.
Once that lands, reproduce below to confirm, then delete this quest.

## Waiting

- mbx or the dotfiles mbx config: stop a test's queue wait for a scheduler permit from counting toward nextest's timeout (for example, take the permit before nextest starts the test's clock, or keep `scheduler.tests` off for nextest). Check with the one-core reproduction from [moq-dev/moq#9163](https://github.com/moq-dev/moq/pull/9163): run `cargo nextest run -p moq-srt --lib --test-threads 4 --stress-count 20` with `KIO_DENY_HOST_TIMERS=1` inside `systemd-run --user --scope -p CPUQuota=100%`, with no `MBX_SCHEDULER_TESTS` override. Before the fix about 20 tests time out at 60s, each printing `mbx: waiting for machine-wide scheduler capacity`; after it, all pass.
