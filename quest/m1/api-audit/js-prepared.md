# [S] JS prepared answers

## Goal

A `Prepared` answer in `@moq/net` owns its request the way Rust's does:
closing it, disposing it, or closing its broadcast before `publish()` refuses
the request, idempotently. A leaked or abandoned `Prepared` never blocks the
path, so the next requester reaches the handler again.

## Plan

Decided 2026-10-22 (API audit). Reproduced: after `prepare()` the request is
marked done, so the iterator's drop and `request.reject()` are no-ops. A
leaked `Prepared` stays pending, a second requester for the path joins it,
and the handler is never asked again until every requester or the claim
closes (`js/net/src/origin.ts`, `prepare`, `Prepared`, `ServeState.enqueue`).

- Replace `Prepared.reject(err)` with an idempotent `close()` plus
  `[Symbol.dispose]` (`Bandwidth.Reservation` is the precedent), mirroring
  Rust where dropping a `Prepared` refuses with `Unroutable`.
- Closing `prepared.broadcast` before `publish()` abandons the request too.
- Make the `Prepared` constructor private with a static factory, and move
  `Request.publisher`, `publishPrepared`, and `rejectPrepared` into module
  hooks (the `finishRequest` pattern), so callers cannot bypass the done
  state or the "forwarded requests cannot be prepared" check.
- Update `doc/lib/js/net.md`, the `js/net/MIGRATE.md` "A claim answers by
  publishing" entry, and `origin-request.test.ts`; examples use `using`.

Test: a handler that throws after `prepare()` leaves the path answerable by
the next request. moq.pro has no JS claim handlers.

Public API: `@moq/net` breaking (`Prepared.reject` to `close`). Wire: none.
