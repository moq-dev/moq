# [XS] js/net's locally closed track test holds under load

## Goal

`js/net`'s "ends a locally closed track cleanly" integration test
(`runSessionDeath(..., true)`, draft 17) passes reliably, fixed at its
cause.

## Plan

Failed once in a full `bun test` run under load on #5803 and passed 6/6
alone. No root cause yet: reproduce it under load first, then decide whether
the product or the test races.

Public API: none. Wire: none.
