# [S] The python finish publisher acknowledges every run

## Goal

`just test interop --all --finish` passes its `python -> *` row: the python
publisher prints `finish acknowledged` once the harness writes `complete`.

## Plan

Seen once on 2026-11-17 while validating
[moq-dev/moq#9052](https://github.com/moq-dev/moq/pull/9052): every reader of the
python publisher passed (`health=ok`), the relay refused nothing, yet the
publisher (`test/interop/clients/python/interop.py`, the `finish_ack` branch)
printed nothing after `finish ready linger_us=` and its session stayed open
until the harness reaped it about two minutes later. A rerun of the row passed.
So it hung after announcing `<broadcast>.health`: in `acknowledge()` reading
the ack FIFO, or in `client.session.state()`. Trace which, under load, and fix
that cause rather than the harness timeout.

Public API: none. Wire: none.
