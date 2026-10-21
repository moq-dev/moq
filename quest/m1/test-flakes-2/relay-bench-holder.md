# [XS] The relay bench's foreign-listener test fails fast on a port collision

## Goal

`bench/relay.test.py`'s `test_a_foreign_listener_never_proves_relay_readiness`
never hangs. Today its `hold.py` binds the port `start_relay` drew at random;
when that port is already taken, `hold.py` dies with EADDRINUSE before writing
the `.ready` fifo, and the harness blocks reading it until the 15 s
`communicate` timeout.

## Plan

Seen during the 2026-10-20 merge batch.

- A holder that cannot bind must reach the harness at once, never by
  leaving a reader blocked on a fifo nobody will open. Either `hold.py` binds
  a port it can own (the kernel's choice) and reports it, or its failure
  writes to `.ready` so `relay_config` sees it, whichever keeps the test
  proving that a foreign listener on the relay's port never counts as
  readiness.
- Check `bench/relay.sh`'s real `start_relay` for the same shape: a relay
  that exits before readiness must end the wait, not time it out.

Public API: none. Wire: none.
