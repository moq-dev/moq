---
title: Development Setup
description: Setting up your development environment
---

# Development Setup

This guide covers setting up a rad development environment.
All of this is optional but is my recommended setup.

And of course, check out the [Quick Start](/setup) first.

## Development Commands

I use [Just](https://github.com/casey/just) to run helper commands.
It's just a fancier `Makefile` so you don't have to remember all the commands.

### Common Commands
```bash
# List all available commands
just

# Run the demo
just dev

# This is equivalent to 3 terminal tabs:
# just relay
# just web
# just pub bbb

# Make sure the code compiles and passes linting
just check

# Auto-fix linting errors
just fix

# Run the tests
just test
```

All of the commands default to `http://localhost:4443/anon`.
You can target a different host by changing the first argument:

```bash
# WARNING: All of these commands use a public relay.
# Anything you publish is publicly visible and accessible.
# Contact @kixelated if you want an authenticated endpoint for sensitive content.

# Run the web server, pointing to the public relay
just web https://cdn.moq.dev/anon

# Publish Tears of Steel, watch it via https://moq.dev/watch?name=tos
just pub tos https://cdn.moq.dev/anon

# Publish a clock broadcast
just clock publish https://cdn.moq.dev/anon

# Subscribe to a clock broadcast
just clock subscribe https://cdn.moq.dev/anon
```

## Debugging

### Rust
You can set the logging level with the `RUST_LOG` environment variable.

```bash
# Print the most verbose logs
RUST_LOG=trace just dev
```

If you're getting a panic, use `RUST_BACKTRACE=1` to get a backtrace.

```bash
# Print a backtrace on panic.
RUST_BACKTRACE=1 just dev
```


## IDE Setup

I use [Cursor](https://www.cursor.com/) and [VSCode](https://code.visualstudio.com/), but anything works.

Recommended extensions:

- [rust-analyzer](https://marketplace.visualstudio.com/items?itemName=rust-lang.rust-analyzer)
- [Biome](https://marketplace.visualstudio.com/items?itemName=biomejs.biome)
- [EditorConfig](https://marketplace.visualstudio.com/items?itemName=EditorConfig.EditorConfig)
- [direnv](https://marketplace.visualstudio.com/items?itemName=mkhl.direnv)


## Contributing

Just make sure to run `just fix` before pushing your changes, otherwise CI will yell at you.

## What's Next?
[P R O D U C T I O N](/setup/production)
