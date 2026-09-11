# [S] Expose WebSocket fallback policy through native bindings

## Goal

A native binding consumer can explicitly select the supported WebSocket fallback
policy without changing the transport connection race's error semantics.

## Plan

Confirm a concrete consumer and extend the existing connection configuration,
mirroring naming across supported bindings. Reuse the transport's policy type;
do not add backend-specific booleans to every wrapper. Verify policy reaches the
client and test each supported combination. This additive capability is separate
from the M0 auth-race fix; classify any actual published API break when scoped.

## Related

- [Connect auth race](/quest/m0/3532-connect-auth-race.md)
