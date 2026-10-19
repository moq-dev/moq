# Migrating moq-boy

How to update your invocation and config for each breaking change, newest first.

## Unreleased

### Build with `--features openh264` (#5771)

moq-boy encodes with OpenH264, which is now opt-in, and refuses to start without it.
Install with `cargo install moq-boy --features openh264`. The Nix package already enables it.
