---
title: Development
description: Build, test, and debug the MoQ repository
---

# Development

The repository uses [Just](https://github.com/casey/just) as its command
runner. Run commands inside the Nix dev shell (`nix develop`) so your tools
match CI.

| Command | Purpose |
| --- | --- |
| `just` | Start the local relay, test publisher, and web demo. |
| `just --list` | List every recipe. |
| `just doctor` | Report which checks this machine can actually run. |
| `just fix` | Format and lint the packages this branch changed. |
| `just check` | Compile and lint the same scope. This is what CI runs. |
| `just test` | Run tests for the same scope. |
| `just fix-all`, `just check-all`, `just test all` | The same, over every package. |
| `just pub bbb <url>` | Publish Big Buck Bunny (also `tos`, `clock`, `gst`, `hls`). |
| `just sub gst bbb <url>` | Play a broadcast through GStreamer. |
| `just relay` | Run a local relay on its own. |
| `just boy` | Run the [MoQ Boy](/bin/demo) demo. |

Recipes default to the local relay at `http://localhost:4443`. Pass
`https://cdn.moq.dev/anon` to use the public relay instead. The default BBB/TOS
publishers and `just pub serve` use MPEG-TS, with one audio frame per PES to
avoid batching latency. Use `just pub cmaf` only when testing fMP4/CMAF.

## Checking your environment

`just check` and `just test` skip whatever isn't installed, so a green run on an
incomplete toolchain looks exactly like a complete one. `just doctor` says which
suites this checkout can actually run, and why one cannot:

```bash
just doctor                  # the scope this branch selects
just doctor --suite all      # everything, including the smoke and wasm harnesses
just doctor --json           # the same results for a script
```

It reports the base and scope it picked, every tool's path and version, the dev
shell it is in, the Cargo wrapper and target directory, writable scratch space
and disk headroom, and whether Nix, a trivial compile, a loopback socket, the
pinned Playwright browser, and GitHub reads work. Each result is `ok`, `missing`,
`denied`, `timeout`, `degraded` (installed, wrong version or behavior), or
`skip`, and each names the suites it blocks.

Every probe is bounded, so an unreachable network or a hung daemon costs seconds.
Nothing is installed or approved for you: the output names the narrow path,
socket, or capability that is missing.

`MOQ_STRICT=1` turns a missing tool into an error rather than a warning, in
`just doctor` and in `just check` alike. CI sets it, so nothing there passes by
skipping.

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
winget: Git, Rust, Bun, Node, just, CMake, and the Visual Studio Build Tools.
Run it from an Administrator terminal on a fresh machine, and re-run it after
reopening the terminal if it reports tools missing from `PATH`.

Run `just` recipes from **Git Bash**, not PowerShell or `cmd`: they need
`bash` and `cygpath`. Only one `just dev` can run at a time on Windows, because
the free-port probe needs `lsof`. If a rebuild fails with "Access is denied",
a previous relay is still running:

```bat
taskkill /IM moq-relay.exe /F
taskkill /IM moq.exe /F
```

## Before opening a pull request

```bash
just fix
just check
just test
```

See [CONTRIBUTING.md](https://github.com/moq-dev/moq/blob/main/CONTRIBUTING.md)
for branch targeting, commit messages, and reviews, and [Agent setup](/setup/agent)
if an AI coding agent is doing the work.
