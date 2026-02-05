# DEPRECATED: Use `./x` or `cargo x` instead.
# This justfile forwards all commands to the new task runner.

[positional-arguments]
[no-cd]
@_default *args:
    cargo x {{args}}
