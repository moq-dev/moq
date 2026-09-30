# [M] Import quinn as moq-quic

## Goal

`rs/moq-quic`, `rs/moq-quic-udp`, and `rs/moq-quic-tokio` are quinn-proto,
quinn-udp, and quinn from quinn-rs/quinn `main`, and they build and pass their
own tests in `just check` and CI. Nothing consumes them yet.

## Plan

Keep the first commit verbatim at a recorded quinn commit so a reviewer can
diff it against upstream; renames, workspace lints, and edition fixes go in
separate commits. Record the parent commit in `rs/moq-quic/README.md` with the
advisory triage process the [questline](/quest/m1/quic/fork/README.md)
describes.

Carry [quinn#2724](https://github.com/quinn-rs/quinn/pull/2724) as its own
cherry-pick commit (the same fix as noq#746, decided 2026-09-30): when the
kernel rejects a GSO batch with EIO or EINVAL, quinn-udp disables GSO and
drops the batch, so an Android client's Initial waits a full RTO and the
WebSocket fallback wins the race. Resend the batch instead. List it as carried
in `rs/moq-quic/README.md`; drop it if upstream lands
[quinn#2748](https://github.com/quinn-rs/quinn/pull/2748) and we cherry-pick
that. moq-uring's own UDP path (`rs/moq-uring/src/udp.rs`) submits GSO
trains itself, so check whether it drops a rejected train the same way, and
fix it here if so.

Keep quinn's own features (qlog, rustls providers, runtimes) as they are;
pruning waits until the [switch](/quest/m1/quic/fork/switch.md) shows what MoQ
uses. Wire the crates into release-plz like the other `rs/` crates, and check
that the added test time fits CI.
