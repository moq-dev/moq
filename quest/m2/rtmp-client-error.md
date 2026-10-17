# [XS] RTMP client reports a publish or play error

## Goal

`moq_rtmp::Client` returns a server's `_error` on publish or play with its
code and description, as it already does on connect. Today the `_error` has
no outstanding transaction, so it is dropped and the client ends with "rtmp
server closed before publish was accepted".

## Plan

Match the publish and play `_error` in `rs/moq-rtmp`'s client and surface it
through `dial.rs`. Test against a server that refuses a publish.
