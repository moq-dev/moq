# [S] Interop runner withdraws publications before disconnecting

## Goal

A completed publish in moq-interop-runner gracefully closes its session so a
subsequent run can publish the same namespace without an already-published error.

## Plan

The runner found this in [#4209](https://github.com/moq-dev/moq/pull/4209).
Replace successful publication's immediate abort with the graceful close API
once the runner uses a moq-net revision that withdraws namespaces on close.
Exercise successive runs against the same relay.

The caller lives outside this repository. Obtain maintainer approval before
posting to its repository; that approval has not been given.
