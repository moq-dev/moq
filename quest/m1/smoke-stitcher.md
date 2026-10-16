# [S] Exercise stitcher-moq with an authorized smoke namespace

## Goal

The public relay matrix exchanges media through the moq-dev stitcher-moq
deployment on every transport it actually serves, using a broadcast namespace
that the deployment permits anonymous smoke publishers to announce.

## Plan

The 2026-10-15 matrix publishes randomly named root broadcasts such as
`smoke-rust-*.hang`. The stitcher-moq deployment permits anonymous publishing
only below `moq-test`, `moq-test-00`, and the one-element `moq-test/interop`
namespace, so every publisher connects but no subscriber can resolve its test
broadcast. Its WebSocket cell also receives a non-WebSocket content type even
though the smoke harness currently opts stitcher-moq into its WebSocket binding.

Reproduce the failure against the public deployment, then make the matrix use a
unique allowed namespace for this endpoint without weakening the other relay
cells. Verify WebTransport and raw QUIC. Keep a WebSocket result only if the
deployment serves that binding; otherwise correct the first-party capability
declaration rather than treating a different service as a failed exchange.

The smoke harness lives in `moq-dev/smoke`. Keep endpoint-specific policy close
to the endpoint metadata instead of teaching the generic publisher about one
deployment. Do not add retries or widen media timeouts.

Public API: none. Wire: none.

