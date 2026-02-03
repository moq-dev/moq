---
title: Development Guide
description: Set up the rest of the stuff.
---

# Development
Still here? You must be a Big Buck Bunny fan.

This guide covers the rest of the stuff you can run locally.

## Just
We use [Just](https://github.com/casey/just) to run helper commands.
It's *just* a fancier `Makefile` so you don't have to remember all the commands.

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

# Publish a HLS broadcast (CMAF) over MoQ
just pub-hls tos
```

Want more? See the [justfile](https://github.com/moq-dev/moq/blob/main/justfile) for all commands.

### The Internet
Most of the commands default to `http://localhost:4443/anon`.
That's pretty lame.

If you want to do a real test of how MoQ works over the internet, you're going to need a remote server.
Fortunately I'm hosting a small cluster on Linode for just the occasion: `https://cdn.moq.dev`

::: warning
All of these commands are unauthenticated, hence the `/anon`.
Anything you publish is public and discoverable... so be careful and don't abuse it.
[Setup your own relay](/setup/prod) or contact `@kixelated` for an auth token.
:::

```bash
# Run the web server, pointing to the public relay
# NOTE: The `bbb` demo on moq.dev uses a different path so it won't show up.
just web https://cdn.moq.dev/anon

# Publish Tears of Steel, watch it via https://moq.dev/watch?name=tos
just pub tos https://cdn.moq.dev/anon

# Publish a clock broadcast
just clock publish https://cdn.moq.dev/anon

# Subscribe to said clock broadcast (different tab)
just clock subscribe https://cdn.moq.dev/anon

# Publish an authentication broadcast
just pub av1 https://cdn.moq.dev/?jwt=not_a_real_token_ask_for_one
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

I use [Cursor](https://www.cursor.com/), but anything works.

Recommended extensions:

- [rust-analyzer](https://marketplace.visualstudio.com/items?itemName=rust-lang.rust-analyzer)
- [Biome](https://marketplace.visualstudio.com/items?itemName=biomejs.biome)
- [EditorConfig](https://marketplace.visualstudio.com/items?itemName=EditorConfig.EditorConfig)
- [direnv](https://marketplace.visualstudio.com/items?itemName=mkhl.direnv)


## Contributing

Run `just fix` before pushing your changes, otherwise CI will yell at you.
It runs `just check` so that's the easiest way to debug any issues.

Please don't submit a vibe coded PR unless you understand it.
`You're absolutely right!` is not always good enough.


## Onwards
`just dev` runs three processes that normally, should run on separate hosts.
Learn how to run them [in production](/setup/prod).

Or take a detour and:
- Brush up on the [concepts](/concept/).
- Discover the other [apps](/app/).
- `use` the [Rust crates](/rs/).
- `import` the [Typescript packages](/js/).
- or IDK, go take a shower or something while Claude parses the docs.
