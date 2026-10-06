# [S] Relay crash drill rebinds its address under load

## Goal

`relay_killed_mid_group_aborts_then_resumes` reliably restarts its relay on
its original UDP address in the concurrent real-QUIC suite. The intermittent
`Address already in use` failure is reproduced and fixed at its cause,
while the interrupted group's failure and both clients' reconnect assertions
remain meaningful.

## Plan

Found during #4928's loaded `just check` on 2026-10-06, in unchanged code:
the impaired lane failed to load the replacement relay after recording that
the interrupted group aborted. Seed `16515990072133439138` failed in the
suite but passed alone, and a later full check passed. A passing rerun is
not evidence that the cause is gone.

- Reproduce under concurrent checks and capture which socket owns the address
  between `RelayHost::kill` and restart. Distinguish retained ownership after
  runtime shutdown from a competing ephemeral bind; do not assume the seed
  alone reproduces a scheduling race.
- Fix the ownership or allocation race at its source. Preserve the crash
  semantics and real reconnect coverage, rather than substituting graceful
  shutdown, retrying the bind, sleeping, or widening timeouts.
- Follow this questline's event-based testing rules. Add a regression that
  fails without the fix and run the existing drill in both lanes through CI.
  Coordinate with the socket-close work if the cause is in the endpoint;
  do not expand this quest into that API refactor without evidence.

Decided during the 2026-10-06 quest-complete sweep: the maintainer selected
separate investigation, then authorized the recommended course unattended.
Keep #4928's documentation correction focused. This is an m1 child of the
existing load-flake questline because it shares that validation goal.

Public API: none expected. Wire: none. Any discovered product impact belongs
in the implementation PR's Impact section. No new user-facing guide is needed
for this fixture investigation; update any comments the fix makes stale inline.

## Related

- [Socket close](/quest/m2/noq-socket-close.md) - replaces the closable socket wrapper, rather than assuming runtime shutdown released it
