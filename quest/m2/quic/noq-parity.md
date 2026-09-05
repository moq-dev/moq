# [M] noq parity gate and quiche retirement

## Goal

noq is the only QUIC core the relay ships by default, on both the tokio and
`moq-uring` paths, with measured evidence that it matches the quiche backend
on the relay workloads and a recorded list of anything a browser can do
against quiche that it cannot do against noq. The production dependency on the
moq-dev/quiche fork is gone.

## Plan

Most of the port already landed: `moq-tokio` defaults to noq, `moq-uring` has
a noq-proto backend sharing the worker, UDP, and WebTransport layers with its
Quinn and quiche backends, and the merge recorded matched throughput with a
lower RSS. What remains is the gate that lets quiche go.

Benchmark chat, media 1:1, media fanout, handshake churn, and idle-connection
memory with `just bench`, on the same worker and crypto configuration, across
four cells: noq and quiche on `moq-uring`, and noq and quinn on the tokio
workers. noq on both runtimes is the matched control #3124 asked for, since
quiche cannot serve per-core tokio workers; the report states which QUIC
stack each runtime mode ran, so runtime and stack are never confounded again.
Adoption requires no material latency, capacity, or RSS regression and a written explanation of any CPU tradeoff.

Run the browser and interop gates on the noq path: `just test wasm`, the
TypeScript interop cases, `just test smoke-full`, and connection draining
during a relay restart. Record each behavior quiche provides that noq does
not. Reliable stream reset is the known one: WebTransport draft 16 requires
`RESET_STREAM_AT` for reset data streams, tracked by the
[reliable reset quest](/quest/m2/quic/reliable-reset.md). Do not switch a
gap off silently; either close it or document it in the backend feature
matrix.

Once the gates pass, remove the production dependency on the moq-dev/quiche
fork, delete fork-only configuration, and keep the ordinary quiche backend
only if it still provides a supported user-selectable path. Update the
backend docs and feature matrix in the same PR.

## Required

- [Reliable stream reset](/quest/m2/quic/reliable-reset.md) - WebTransport
  parity cannot be claimed without it
- [Release the fork stack](/quest/m2/quic/release.md) - the relay must pin
  released noq sources before quiche goes

## Closes

- [#3124](https://github.com/moq-dev/moq/issues/3124) - the parity report is the matched runtime-versus-QUIC-stack measurement the issue asked for
