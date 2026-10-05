# [XS] WebSocket fixed-address tests off the paused clock

## Goal

moq-tokio websocket `fixed_addresses_keep_tls_name_and_request_host` and
`ipv6_literal_fixed_addresses` no longer pause the clock over a real TLS
dial.

## Plan

They pass today, but they pause the clock over a real TLS dial on loopback.
Once a timeout lands on that path, the paused clock can fire it before
loopback delivers, the race [#4527](https://github.com/moq-dev/moq/pull/4527)
removed from the auth outage tests. Remove it the same way.
