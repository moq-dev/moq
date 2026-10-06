# [M] tls:// peers can authenticate by certificate

## Goal

A relay's `tls://` (qmux over TLS on TCP) listener can identify a cluster
peer by its client certificate, as the QUIC listener does, so an upstream
link doesn't need a token.

## Plan

#4816 added `tls://` with no client certificate request, so peers present a
token. Requesting one means owning the TLS accept instead of
`qmux::tls::Server`. Keep the token path working on the same listener, and
keep pinned peers and CA roots meaning the same as on QUIC.

Public API: relay config may gain a listener option. Wire: none.
