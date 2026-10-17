# [S] A FIN below received data is a final size error

## Goal

A STREAM frame whose FIN sets a final size below data already received closes
the connection with FINAL_SIZE_ERROR, as RFC 9000 4.5 requires. Today the
frame is accepted: the reader gets the bytes past the final size, then
`Blocked` forever, since the shrunken final size never matches what was
read.

`stream_final_size_below_received` in
`rs/moq-quic/src/tests/characterize/frames.rs` must pass with its `#[ignore]`
removed.

## Plan

`Recv::ingest` in `rs/moq-quic/src/connection/streams/recv.rs` checks a FIN
only against an already-known final size. When a FIN arrives first, also
reject an `end` below `self.end`, before recording the size. RESET_STREAM
already makes this check (`reset_final_size_below_received`).
