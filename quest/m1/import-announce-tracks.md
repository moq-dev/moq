# [S] An importer announces once it knows its tracks

## Goal

An importer starting beside a live partner, such as the second `import ts`
of a pair that has not parsed its PMT yet, announces its broadcast only once
it can serve its tracks, so a relay that queries it the moment it connects
never gets `not found` and never aborts the subscribers already watching the
partner (#4352).

## Plan

Split out of [same-epoch importers](/quest/m1/hop-aligned-import.md)
(2026-10-13): it needs no epoch anchor and lands on its own.

In #4352 the standby connected, the relay queried every track within a
millisecond, the standby answered `not found` for each before parsing its PMT,
and both `export ts` subscribers exited 15 ms later. Delay the announcement in
the importers (`rs/moq-mux` import paths behind `moq import`) until the
catalog lists the tracks the importer will publish: for TS, the first PMT; for
the other formats, whatever first declares their tracks. An importer that
never learns its tracks never announces, and fails loud at its own deadline
rather than announcing an empty broadcast.

Test: the #4352 setup, one relay, a running `import ts` with two `export
ts` subscribers, then a second `import ts` on the same path connecting before
its PMT arrives; both subscribers keep running. Repeat for one other importer
format.

Public API: none expected. Wire: none.

## Closes

- [#4352](https://github.com/moq-dev/moq/issues/4352) - close this issue when the quest finishes

## Related

- [Same-epoch importers publish identical tracks](/quest/m1/hop-aligned-import.md) - the redundant-pair contract this is one piece of
