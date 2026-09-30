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

Keep quinn's own features (qlog, rustls providers, runtimes) as they are;
pruning waits until the [switch](/quest/m1/quic/fork/switch.md) shows what MoQ
uses. Wire the crates into release-plz like the other `rs/` crates, and check
that the added test time fits CI.
