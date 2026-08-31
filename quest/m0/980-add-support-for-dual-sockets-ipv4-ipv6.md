# [M] Add support for dual-sockets (IPv4 + IPv6)

## Goal

Implement and verify the behavior tracked in [#980](https://github.com/moq-dev/moq/issues/980)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

Linux will let you bind to IPv6 and automatically translate any IPv4 packets. However, Windows doesn't?

Additionally, the client currently dials DNS and chooses the first entry. Not only might it be the wrong protocol (we should check), but the server might not actually be listening on that IP.

Unfortunately, I think we should implement happy-eyeballs. Race the IPv4 and IPv6 connections over separate IPv4/IPv6 sockets.

## Closes

- [#980](https://github.com/moq-dev/moq/issues/980) - close this issue when the quest finishes
