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

- Measure first the likeliest cause: the relay's first port comes from
  `relay_config(None)`, so it is ephemeral, and it sits unbound between
  `RelayHost::kill` and the restart. The kill sends no CONNECTION_CLOSE, so
  clients only notice the loss by timeout, and in that window another test's
  `bind(0)` under load can take the port.
- Reproduce under concurrent checks and capture which socket owns the address
  between `RelayHost::kill` and restart. Distinguish that competing ephemeral
  bind from retained ownership after runtime shutdown; do not assume the seed
  alone reproduces a scheduling race.
- Fix the ownership or allocation race at its source. Fixture-level fixes are
  in scope, such as a stable client-facing address (a forwarder, for
  example) so the restarted relay can take a fresh port. Preserve the crash
  semantics and real reconnect coverage; retrying the bind, sleeping,
  widening timeouts, or substituting graceful shutdown stay out.
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
