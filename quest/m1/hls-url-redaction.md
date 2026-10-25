# [XS] HLS import errors never print a signed URL

## Goal

No `moq_hls::Error` prints a playlist or segment URL's query or userinfo, so a
signed token (a `jwt` query, say) never reaches logs or stderr. Today the
`From<reqwest::Error>` conversion strips the URL deliberately, but the byte-range
variants in `rs/moq-hls/src/error.rs` (`MissingByteRangeOffset`,
`InvalidByteRange`, `ByteRangeLengthMismatch`, `ByteRangeResponseMismatch`)
format the full `url::Url`.

## Plan

From the 2026-10-24 moq-hls audit. The leak is to logs only: `moq-cli`'s
`Error::Transport(format!("{err:#}"))` never reaches a subscriber, because
moq-lite sends codes and the IETF paths that send a phrase never see an aborted
track. It is still a credential in logs that are routinely shipped elsewhere.

- Every error variant that names a resource prints it redacted (scheme, host,
  and path; no query, fragment, or userinfo), through one shared display
  helper, so a new variant cannot reintroduce the leak.
- Test that each URL-carrying variant's `Display` and `{:#}` omit a query token
  and userinfo.

Public API: none beyond `Display` text. Wire: none.
