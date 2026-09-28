# [XS] Packaged relay starts

## Goal

The `.deb` and `.rpm` relay service starts. Today
`packaging/moq-relay/moq-relay.service` runs `moq-relay --file
/etc/moq-relay/relay.toml`, but the config path is positional and the relay
rejects unknown flags, so every packaged install crash-loops.

## Plan

`ExecStart=/usr/bin/moq-relay /etc/moq-relay/relay.toml`. Add the reporter's
unit test in the relay crate that parses the unit's `ExecStart` with the
relay's own `Cli`, so the unit and the parser can't drift again.

## Closes

- [#4343](https://github.com/moq-dev/moq/issues/4343) - close this issue when the quest finishes
