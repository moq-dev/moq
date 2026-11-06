# Migrating moq-boy

How to update your invocation and config for each breaking change, newest first.

## Unreleased

### Viewers dial the game's socket endpoint

moq-boy no longer reads viewer broadcasts. It runs a [socket](https://doc.moq.dev/concept/socket) endpoint at `{prefix-game}/.socket/{name}`, and each viewer dials it from its own endpoint at `{prefix-game}/.socket/viewer-<random>`; `@moq/boy` does this.

- Drop `--prefix-viewer` (`MOQ_PREFIX_VIEWER`), which now stops startup: viewers dial `{prefix-game}/.socket/viewer-*`, so only `--prefix-game` remains.
- `--name` is the game's endpoint id, so a name with `/` or `\`, or starting with `viewer-` (ids that belong to viewers), is refused.
- Sockets need moq-lite-07 or moq-transport on every hop. With no `--connect-version`, moq-boy offers only `moq-lite-07-wip`, so the relay must list it in `listen.version`.
- Let the relay grant the emulator publish on `{prefix-game}/.socket/{name}` and subscribe on `{prefix-game}/.socket/*`, and anonymous viewers publish on `{prefix-game}/.socket/viewer-*` in place of the old viewer prefix.
- The `status` track's `latency` map is gone; it carries `players`, the number of open viewer sessions. Each viewer's latency arrives on its own session.

### Unknown `MOQ_*` environment variables stop startup ([#6633](https://github.com/moq-dev/moq/pull/6633))

A `MOQ_*` variable moq-boy does not read, such as a misspelling, a removed name, or one meant for another binary, now stops startup with the variable named instead of being ignored.
Remove or rename what the error lists.

### Key logging is opt-in (#5907)

`SSLKEYLOGFILE` alone no longer writes TLS session secrets.
Add `--quic-keylog` (`MOQ_QUIC_KEYLOG`) to capture them; without `SSLKEYLOGFILE` it stops startup.

### Build with `--features openh264` (#5771)

moq-boy encodes with OpenH264, which is now opt-in, and refuses to start without it.
Install with `cargo install moq-boy --features openh264`. The Nix package already enables it.
