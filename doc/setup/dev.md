---
title: Development
description: Run, test, and debug the MoQ workspace
---

# Development

The repository uses [Just](https://github.com/casey/just) as its command runner.
Run commands inside the Nix development shell when possible so your tools match
CI.

## Common commands

| Command | Purpose |
| --- | --- |
| `just` | Start the local relay, test publisher, and web demo. |
| `just --list` | List available recipes. |
| `just fix` | Format and lint changed packages and their dependents. |
| `just check` | Compile and lint the same changed-package scope. |
| `just test` | Run tests for the same changed-package scope. |
| `just fix-all` | Format and lint every Rust, TypeScript, and Python package. |
| `just check-all` | Compile and lint every package. |
| `just test all` | Test every Rust, TypeScript, and Python package. |

For example, publish the Tears of Steel HLS fixture over MoQ with:

```bash
just pub hls tos
```

The root [justfile](https://github.com/moq-dev/moq/blob/main/justfile) and
`just --list` show the remaining recipes.

## Test over the internet

Most demo commands use the local relay at `http://localhost:4443`. The public
test relay is available at `https://cdn.moq.dev/anon`:

::: warning Public namespace
The `/anon` path is unauthenticated. Broadcasts published there are public and
discoverable. Do not publish private media or rely on a name remaining reserved.
:::

```bash
# Run the local web client against the public relay.
just web serve https://cdn.moq.dev/anon

# Publish Tears of Steel, then open https://moq.dev/watch?name=tos.
just pub tos https://cdn.moq.dev/anon

# Publish and subscribe to a data-only clock broadcast in separate terminals.
just pub clock publish https://cdn.moq.dev/anon
just pub clock subscribe https://cdn.moq.dev/anon
```

For private paths, deploy a relay and configure
[authentication](/bin/relay/auth).

## Debug Rust

Use `RUST_LOG` for structured logs and `RUST_BACKTRACE` for panic backtraces:

```bash
RUST_LOG=trace just
RUST_BACKTRACE=1 just
```

## Editor setup

Any editor with standard Rust and TypeScript support works. Useful extensions
include:

- [rust-analyzer](https://marketplace.visualstudio.com/items?itemName=rust-lang.rust-analyzer)
- [Biome](https://marketplace.visualstudio.com/items?itemName=biomejs.biome)
- [EditorConfig](https://marketplace.visualstudio.com/items?itemName=EditorConfig.EditorConfig)
- [direnv](https://marketplace.visualstudio.com/items?itemName=mkhl.direnv)

## Before opening a pull request

Run the same checks used by CI:

```bash
just fix
just check
just test
```

These commands scope work to packages changed from the branch's configured
upstream and include their dependents. Use `just fix-all`, `just check-all`, and
`just test all` when changing shared tooling or configuration that the package
diff cannot attribute. `just check-all` also covers language wrappers whose
tests are part of their check recipe.

See [CONTRIBUTING.md](https://github.com/moq-dev/moq/blob/main/CONTRIBUTING.md)
for branch targeting, commits, and pull requests.
