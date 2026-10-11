# [S] Drain in-flight handshakes

## Goal

A relay drain counts a session from admission, not only once it is
established, so a client still in its handshake when the drain starts is sent
a GOAWAY and waited for instead of being cut off by the relay exiting. The
drain still ends at its deadline whatever a handshake does.

## Plan

The implementation shares `moq_net::server::Activity` across native server
configuration, relay shutdown, and io_uring endpoint configuration. It reuses
kio producer/consumer ownership instead of maintaining another integer counter.
Native accepted transports acquire a guard before their asynchronous handshake;
Request owns it through authorization and transfers it to the session driver.
The HTTP WebSocket path owns it before auth and through the upgrade. io_uring
owns it from accepting the Initial through terminal connection failure, including
failed TLS handshakes. Outbound dials do not count as admitted connections.

Auth refusal, handshake failure, and cancellation release ownership immediately.
An unfinished handshake is bounded by the original absolute drain deadline;
the former extra second is removed. Established-session metrics keep their
existing meaning. The io_uring drain regression uses a held auth admission
instead of an unrelated old-version peer and polling sleeps.

The pending-auth regression failed before the fix and passed afterward. Native
TCP/Unix tests cover the pre-SETUP handshake and cancellation; a late auth grant
receives GOAWAY after the drain starts. A mocked-time regression demonstrates
that an unfinished handshake cannot extend the fixed deadline. The endpoint
close test checks incoming ownership before accept, excludes outgoing dials,
and releases ownership while closed handles still survive. Both io_uring regressions executed and passed on the validation host; report
them as skipped on hosts without kernel support. Full Nix `just check` passed.
All-language interop and the worker-count/admitted-connection benchmark passed.
The remaining blocker is the public API placement decision below.

The public API placement is provisional, so keep this quest and PR draft until
it is settled:

1. Keep the implemented portable `server::Activity` / `Active` guard in moq-net.
   Recommended: both runtimes already share this dependency and ownership model.
2. Put a general activity handle in kio, broadening its reusable public surface.
3. Keep separate runtime counters and specify atomic admission/idle composition
   at relay shutdown. Independently awaiting counters is insufficient if a new
   admission arrives after one wait has already resolved.

Counting only returned Requests misses the transport handshake. A relay
callback adds another public boundary without removing the lifetime requirement.
No wire format, negotiation, assigned values, or FFI ABI changes are intended.
