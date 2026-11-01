---
title: Development
description: Build, test, and debug the MoQ repository
---

# Development

The repository uses [Just](https://github.com/casey/just) as its command
runner. Run commands inside the Nix dev shell (`nix develop`) so your tools
match CI. `just check` and `just fix` refuse to run outside it; set
`ALLOW_HOST_TOOLCHAIN=1` to use the host toolchain anyway.

| Command | Purpose |
| --- | --- |
| `just` | Start the local relay, test publisher, and web demo. |
| `just --list` | List recipes; use `just --list test` or another module to narrow the list. |
| `just ci-select` | Report whether CI and Rust build setup are needed, without running checks. |
| `just fix` | Format and lint the packages this branch changed, and regenerate their committed JS API reports (`js/*/api/`). |
| `just check` | Compile, lint, and test the same scope. |
| `just ci` | Run the batched landing gate locally, strictly. PRs into `main` get no other CI, so run it before every push. |
| `just gate` | `just ci` over every package, as nightly and release PRs run it. `just gate rs` runs only the named module. |
| `just fix --all`, `just check --all` | The same, over every package. |
| `just api-diff` | Diff every published API against its last release, the JS reports against ones rendered from `origin/release`. PRs into `release` run it; `--deny` fails a backport on any break. |
| `just pub bbb <url>` | Publish Big Buck Bunny (also `tos`, `clock`, `gst`, `hls`). |
| `just sub gst bbb <url>` | Play a broadcast through GStreamer. |
| `just relay` | Run a local relay on its own. |
| `just boy` | Run the [MoQ Boy](/bin/demo) demo. |

Recipes default to the local relay at `http://localhost:4443`. Pass
`https://cdn.moq.dev/anon` to use the public relay instead. `just pub bbb`
publishes the original 720p video plus a pre-encoded 360p rendition, so the
player can switch between them without encoding while publishing.

## Find your way around

The [repository map](/setup/repository) links each feature to its implementation
and tests. [Examples](/setup/examples) locates existing consumers to run or
adapt. Commands below run from the repository root.

## Checks and tests

`just check` selects affected packages and their dependents from the branch
diff, including local changes. Each language runner prints its selected
packages when it starts. Pass a base explicitly, such as
`just check origin/main`, when needed. Independent modules finish even when
another fails, and any failure keeps the run failing. On `x86_64` Linux, if the
local Nix builder cannot create user namespaces, `just check` reports the flake
module as skipped; `just ci` still requires the full flake check.
[sh/dispatch.sh](https://github.com/moq-dev/moq/blob/main/sh/dispatch.sh)
contains the module selection, with package selection delegated to the
language runners.

| What you changed or need to measure | Start here |
| --- | --- |
| Ordinary source changes | `just check`; [Rust instructions](https://github.com/moq-dev/moq/blob/main/rs/AGENTS.md) and [TypeScript instructions](https://github.com/moq-dev/moq/blob/main/js/AGENTS.md) describe focused test commands and conventions |
| Wire behavior, FFI, or gateways | `just test interop --all`; the [interop guide](https://github.com/moq-dev/moq/blob/main/test/interop/README.md) describes language and version selection |
| Browser capture and playback | The [web demo](/bin/demo) for interactive checks; `just test media` for the browser media harness |
| Transport failure and recovery | [Failure drills](https://github.com/moq-dev/moq/blob/main/test/drill/README.md) |
| Compatibility with published packages | [Wire compatibility](https://github.com/moq-dev/moq/blob/main/test/interop/README.md) and [registry smoke tests](https://github.com/moq-dev/moq/blob/main/test/smoke/README.md) |
| Throughput, latency, fan-out, or memory costs | [Benchmark guide](https://github.com/moq-dev/moq/blob/main/bench/README.md) |

The [test index](https://github.com/moq-dev/moq/blob/main/test/README.md) covers
other harnesses and where failed runs keep logs. A plain `just test interop`
selects Rust only; use `--all` for the cross-language matrix. Each harness's
README describes its prerequisites and scope.

## Editing documentation

Edit the feature page under `doc/`; use
[doc/AGENTS.md](https://github.com/moq-dev/moq/blob/main/doc/AGENTS.md) for the
boundary between an overview and a reference. For a new page, give it a title
and description, link it from the relevant index, and add it to the sidebar in
`doc/.vitepress/config.ts` so readers and agents can find it.

```bash
bun run --cwd doc dev       # preview the site
bun run --cwd doc check     # type-check, test, and build, including dead-link checks
```

`just check` includes the documentation build when the site changes. The build
also generates the [markdown pages and agent index](/setup/agent#reading-the-docs).
Edit protocol sources in `drafts/`; `doc/draft/` is generated from them.
The [JS API reference](/lib/js/api/) is generated from the packages' TSDoc; the dev
server reuses the last one until a package changes, or always with
`MOQ_API_REFERENCE=regenerate`.
Package API documentation stays beside the source. If you change a binding's
code sample, its language check compiles the sample too.

## Debugging

```bash
RUST_LOG=debug just            # structured logs
RUST_LOG=moq_net=trace just    # one crate
RUST_BACKTRACE=1 just          # panic backtraces
```

The relay's [HTTP endpoints](/bin/relay/http) list announced broadcasts and
fetch groups with `curl`, which is the quickest way to see what a relay holds.

## Windows

Nix isn't available on Windows, so `setup.bat` installs the toolchain with
winget: Git, Rust, Bun, Node, just, CMake, FFmpeg, and the Visual Studio Build Tools.
Run it from an Administrator terminal on a fresh machine, and re-run it after
reopening the terminal if it reports tools missing from `PATH`.

`bun install` normally builds `@moq/libav`, the Opus and AAC decoder behind the
WebCodecs fallback, with Nix. On Windows it downloads the published package of
the same version instead, and fails until npm has that version.

Run `just` recipes from **Git Bash**, not PowerShell or `cmd`: they need
`bash`. Only one `just dev` can run at a time on Windows, because
the free-port probe needs `lsof`. If a rebuild fails with "Access is denied",
a previous relay is still running:

```bat
taskkill /IM moq-relay.exe /F
taskkill /IM moq.exe /F
```

## Before opening a pull request

```bash
just fix
just ci
```

`just ci` runs the batched landing gate for the selected scope. See
[Checks and tests](#checks-and-tests) for targeted checks while iterating.

See [CONTRIBUTING.md](https://github.com/moq-dev/moq/blob/main/CONTRIBUTING.md)
for branch targeting, commit messages, and reviews, and [Agent setup](/setup/agent)
if an AI coding agent is doing the work.
