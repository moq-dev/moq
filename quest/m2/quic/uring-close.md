# [M] Deliver the application close before io_uring teardown

## Goal

Make `moq-uring::teardown::a_published_close_has_already_left_the_client`
consistently deliver the application close code and reason when the client
stops its worker immediately after `poll_closed` completes.

During local validation of [#3649](https://github.com/moq-dev/moq/pull/3649),
`just test` passed 4,082 tests but this test failed after 10.4 seconds: the
server saw `connection timed out` instead of application close code 42 and
reason `done`. The isolated run passed in 0.1 seconds. The PR did not change
`rs/moq-uring` or its teardown test.

Reproduce on a Linux kernel supporting the io_uring worker, both in isolation
and alongside the full test suite. Determine where the final close is lost
between QUIC output, UDP submission, and worker teardown. Preserve the existing
close contract; do not mask the failure with a retry or a longer timeout.
