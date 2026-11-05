# Migrating moq-boy

How to update your invocation and config for each breaking change, newest first.

## Unreleased

### Unknown `MOQ_*` environment variables stop startup ([#6633](https://github.com/moq-dev/moq/pull/6633))

A `MOQ_*` variable moq-boy does not read, such as a misspelling, a removed name, or one meant for another binary, now stops startup with the variable named instead of being ignored.
Remove or rename what the error lists.

### Key logging is opt-in (#5907)

`SSLKEYLOGFILE` alone no longer writes TLS session secrets.
Add `--quic-keylog` (`MOQ_QUIC_KEYLOG`) to capture them; without `SSLKEYLOGFILE` it stops startup.

### Build with `--features openh264` (#5771)

moq-boy encodes with OpenH264, which is now opt-in, and refuses to start without it.
Install with `cargo install moq-boy --features openh264`. The Nix package already enables it.
