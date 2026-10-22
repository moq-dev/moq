# [S] ACME renewal honors the failed-validation budget

## Goal

A persistent renewal misconfiguration cannot lock the relay's hostname out of
Let's Encrypt. Renewal counts failed validations against the same budget
startup uses, and pauses once it is spent while the previous certificate
keeps serving.

## Plan

Decided 2026-10-22 from the security audit, with the maintainer.
`Acme::issue` (`rs/moq-relay/src/acme.rs`) records each
`Order(BadAuth)` and refuses after `FAILURE_LIMIT` within `FAILURE_WINDOW`,
which keeps the relay under five failed validations per hostname per hour.
`Acme::run` only logs errors, and rustls-acme 0.15 retries a failed order
after 1, 2, 4, 8, and 16 seconds, so renewal can exceed the limit within a
minute. Renewal failures are also not persisted, so a restart after them
misjudges the budget.

- In `run`, record each `BadAuth` through `self.failures`, as `issue`
  does.
- Once the recent failures reach the limit, stop polling the ACME state until
  the oldest failure leaves the window, then resume. Not polling the
  stream pauses rustls-acme's order loop without forking it. Log the pause
  with the time renewal resumes.
- A successful renewal clears the failures, as issuance does.

Test (mocked time): repeated renewal `BadAuth` failures stop at the limit,
resume after the window, and are seen by a later `issue` call.

Docs: `doc/bin/relay/config.md` says the budget covers renewal. Public API:
none. Wire: none.
