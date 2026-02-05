---
title: Development Guide
description: Set up the rest of the stuff.
---

# Development
Still here? You must be a Big Buck Bunny fan.

This guide covers the rest of the stuff you can run locally.

## Task Runner
We use a Rust-based task runner ([rs/x](https://github.com/moq-dev/moq/blob/main/rs/x)) to run helper commands.
Invoke it via `./x` or `cargo x`.

### Common Commands
```bash
# List all available commands
./x --help

# Run the demo
./x dev

# This is equivalent to 3 terminal tabs:
# ./x relay
# ./x web
# ./x pub fmp4 bbb

# Make sure the code compiles and passes linting
./x check

# Auto-fix linting errors
./x fix

# Run the tests
./x test

# Publish a HLS broadcast (CMAF) over MoQ
./x pub hls tos
```

Want more? Run `./x --help` or see [rs/x/src/main.rs](https://github.com/moq-dev/moq/blob/main/rs/x/src/main.rs) for all commands.

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
./x web https://cdn.moq.dev/anon

# Publish Tears of Steel, watch it via https://moq.dev/watch?name=tos
./x pub fmp4 tos https://cdn.moq.dev/anon

# Publish a clock broadcast
./x clock publish https://cdn.moq.dev/anon

# Subscribe to said clock broadcast (different tab)
./x clock subscribe https://cdn.moq.dev/anon

# Publish an authentication broadcast
./x pub fmp4 av1 https://cdn.moq.dev/?jwt=not_a_real_token_ask_for_one
```

## Debugging

### Rust
You can set the logging level with the `RUST_LOG` environment variable.

```bash
# Print the most verbose logs
RUST_LOG=trace ./x dev
```

If you're getting a panic, use `RUST_BACKTRACE=1` to get a backtrace.

```bash
# Print a backtrace on panic.
RUST_BACKTRACE=1 ./x dev
```


## IDE Setup

I use [Cursor](https://www.cursor.com/), but anything works.

Recommended extensions:

- [rust-analyzer](https://marketplace.visualstudio.com/items?itemName=rust-lang.rust-analyzer)
- [Biome](https://marketplace.visualstudio.com/items?itemName=biomejs.biome)
- [EditorConfig](https://marketplace.visualstudio.com/items?itemName=EditorConfig.EditorConfig)
- [direnv](https://marketplace.visualstudio.com/items?itemName=mkhl.direnv)


## Contributing

Run `./x fix` before pushing your changes, otherwise CI will yell at you.
It runs `./x check` so that's the easiest way to debug any issues.

Please don't submit a vibe coded PR unless you understand it.
`You're absolutely right!` is not always good enough.


## Onwards
`./x dev` runs three processes that normally, should run on separate hosts.
Learn how to run them [in production](/setup/prod).

Or take a detour and:
- Brush up on the [concepts](/concept/).
- Discover the other [apps](/app/).
- `use` the [Rust crates](/rs/).
- `import` the [Typescript packages](/js/).
- or IDK, go take a shower or something while Claude parses the docs.
