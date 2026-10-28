# [XS] RTMP client reports a publish or play error

## Goal

`moq_rtmp::Client` returns a server's `_error` on publish or play with its
code and description, as it already does on connect. Today the `_error` has
no outstanding transaction, so the session raises
`UnknownTransactionResultReceived`, `dial.rs` ignores it, and the client ends
with "rtmp server closed before publish was accepted" once the server closes.

## Plan

Match the publish and play `_error` in `rs/moq-rtmp`'s client and surface it
through `dial.rs`. Test against a server that answers a publish with `_error`
and one that answers a play with `_error`, asserting the code and description
in both.
