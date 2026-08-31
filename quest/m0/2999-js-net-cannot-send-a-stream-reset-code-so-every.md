# [S] js/net cannot send a stream reset code, so every cancellation reads as INTERNAL_ERROR

## Goal

Implement and verify the behavior tracked in [#2999](https://github.com/moq-dev/moq/issues/2999)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

Found during the adversarial review of #2993.

`js/net` has no way to send an application stream error code. Every teardown passes a plain `Error`:

- `Stream.close()` -> `writer.close()` + `reader.stop(new Error("cancel"))`
- `Stream.abort(reason)` -> `writer.reset(reason)` + `reader.stop(reason)`
- `Writer.reset(reason)` -> `WritableStreamDefaultWriter.abort(reason)`
- `Reader.stop(reason)` -> `ReadableStreamDefaultReader.cancel(reason)`

WebTransport only carries a code when the reason is a `WebTransportError` with `streamErrorCode` set; nothing in `js/net` ever constructs one. So every reset and STOP\_SENDING goes out as code `0`.

On the IETF wire that matters: draft-19 section 3.3.4 assigns `INTERNAL_ERROR` to `0x0` and `CANCELLED` to `0x1`. A routine browser unsubscribe therefore reaches the publisher looking like a fault on our side, to be handled and counted as one. #2993 fixed this on the Rust side for the cancellation path (`STREAM_CANCELLED` via a raw-code `Reader::stop`), but the JS equivalent needs a code parameter threaded through `Writer.reset` and `Reader.stop` first.

Two things make this bigger than a one-line change, which is why it was left out of #2993:

- It changes a shared abstraction, so the moq-lite path is affected too and both error spaces have to be kept straight.
- `WebTransportError` construction needs a compatibility story for non-browser test environments.

The same widening was declined on the Rust side for the equivalent reason: other IETF paths (`run_uni_group`, for one) still reset with moq-lite codes, and untangling that means reworking `Error::from_transport`, where `0` decodes back to `Cancel`.

## Closes

- [#2999](https://github.com/moq-dev/moq/issues/2999) - close this issue when the quest finishes
