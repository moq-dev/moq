# [S] The leak control detects a leak that shares a pooled connection

## Goal

`just test smoke-media` passes on dev. Its leaked-session negative control
proves the harness can still catch an undetached player when every player on a
relay URL shares one transport, so the control fails on `resource baseline`,
the assertion it names, rather than timing out earlier on `resource
instrumentation`.

Boundaries: the leak signal is the only thing that moves. The positive
`resource baseline` predicate stays as it is, because a detach still has to
release everything, and the pooling behaviour itself is correct and stays.

## Plan

The control leaks a second player and then waits for the process to hold more
sessions than it did while playing
(`test/smoke/clients/js/media.ts:483`, predicate `r.transports + r.sockets >
busy.resources.transports + busy.resources.sockets`). That premise was true on
main, where each `<moq-watch>` dialled its own connection. On dev
`js/watch/src/element.ts:204` takes a `Moq.Connection.Shared` handle, and
`js/net/src/connection/pool.ts` keys the pool on the relay URL, so the leaked
player reuses the transport the first one already holds. The count never
rises, the wait burns `SETTLE_MS`, and the run reports:

```
negative control was aimed at "resource baseline" but broke elsewhere:
resource instrumentation: waiting for the deliberately leaked player to open
another session beyond {"transports":1,...,"audioContexts":1,...}:
{"transports":1,"sockets":0,"audioContexts":2,"workers":0}
```

The leak is real and the instrumentation already sees it: `audioContexts` went
1 to 2. A transport count cannot be the signal under pooling, so the predicate
needs one that a pooled player still moves. `audioContexts` is the obvious
candidate, since `test/smoke/clients/js/src/instrument.ts:71` counts them and
each player builds its own audio graph regardless of which transport it rides.
Prefer a signal that survives [#2774](/quest/m1/2774-collapse-reload-and-shared-into-one-connection-class.md),
which keeps pooling while renaming the class: anything keyed on how many
transports exist will break again there.

Worth deciding while here: whether the pool should expose a handle count, so
the harness can assert on "two handles on one transport" directly instead of
inferring a leak from a side effect. That is a published-surface question, not
just a harness one, which is why it sits in this milestone.

Verification is the harness itself: `just test smoke-media` goes green, and
the control still fails when the leak is removed, which is what
`--leak`/`--expect-fail` already assert.

## Related

- [#2774](/quest/m1/2774-collapse-reload-and-shared-into-one-connection-class.md) - reshapes the pooling this control has to see through
