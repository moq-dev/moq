# [S] Fix the flaky test a-session-close-right-after-a-datagram-tracks-end-keeps-the

## Goal

Fix `@moq/net moq-lite-06: a session close right after a datagram tracks end keeps the end` so it passes reliably, or delete it, and remove its skip.
quest land watch disabled it after the land fixer judged it flaky in
more than one `merge_group` build.

Failing jobs:

- [Job 1](https://github.com/moq-dev/moq/actions/runs/44557764205/job/135678374099)
- [Job 2](https://github.com/moq-dev/moq/actions/runs/44566631708/job/135710065415)
