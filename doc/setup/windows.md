---
title: Windows Setup
description: Install the MoQ build toolchain on Windows and run a local relay
---

# Windows Setup

Nix isn't available on Windows, so the toolchain is installed natively with
[winget](https://learn.microsoft.com/windows/package-manager/winget/). A
[`setup.bat`](https://github.com/moq-dev/moq/blob/main/setup.bat) script at the
repo root automates it.

## Quick start

```bat
git clone https://github.com/moq-dev/moq
cd moq
setup.bat
```

On a fresh machine, run it from an **Administrator** terminal: Git and the
Visual Studio Build Tools install machine-wide and need elevation. The script
is safe to re-run; winget skips or upgrades anything already present. It
installs:

- [Git](https://git-scm.com/), [Rust (rustup)](https://rustup.rs/),
  [Bun](https://bun.sh/), [Node.js LTS](https://nodejs.org/),
  [just](https://github.com/casey/just), and [CMake](https://cmake.org/)
- [GitHub CLI](https://cli.github.com/) and [uv](https://docs.astral.sh/uv/)
  (for the `py/` workspace)
- **Visual Studio Build Tools** with the C++ workload, which provides the MSVC
  linker every Rust MSVC build needs. This step is skipped when the C++ toolset
  is already installed.

It then runs `rustup update stable` and `bun install`.

::: warning Reopen your terminal after a fresh install
Tools installed by winget land on `PATH` only for *new* terminals. If the
script reports that `rustup` or `bun` is "not on PATH yet", close and reopen
your terminal and run `setup.bat` again. The second run finishes the
`rustup update` / `bun install` steps.
:::

## Build and run

```bat
REM Build the Rust workspace
cargo build

REM Run a local relay (self-signed cert, anonymous access)
cargo run --bin moq-relay -- demo/relay/localhost.toml
```

The relay listens on `[::]:4443` for QUIC and serves its certificate
fingerprint over HTTP at <http://localhost:4443/certificate.sha256>. Both
listeners are dual-stack, so IPv4 (`127.0.0.1`) and IPv6 (`[::1]`) both work.

With the relay running, publish and subscribe to a clock in two more terminals:

```bat
REM Grab the relay's certificate fingerprint
for /f %f in ('curl -s http://localhost:4443/certificate.sha256') do set FP=%f

REM Publish
cargo run -p moq-native --example clock -- --url https://localhost:4443/anon --broadcast clock --tls-fingerprint %FP% publish

REM Subscribe (separate terminal)
cargo run -p moq-native --example clock -- --url https://localhost:4443/anon --broadcast clock --tls-fingerprint %FP% subscribe
```

The subscriber prints the current time once per second, sourced from the
publisher through the relay.

## Running the full demo with `just dev`

`just dev` runs the whole demo: a local relay, Big Buck Bunny published through
it via `ffmpeg`, and the web UI at <http://localhost:5173>.

Run it **from a Git Bash terminal** (installed with Git for Windows), not
PowerShell or `cmd`:

```bash
just dev
```

`just` recipes run through a POSIX shell and need `bash`, `sh`, and `cygpath`.
Git Bash provides all three plus the `curl`/`sleep`/`seq` the recipes use, and
inherits `just`/`cargo`/`bun`/`ffmpeg` from your Windows `PATH`. PowerShell and
`cmd` don't work here: the `bash` they find is the WSL stub, and `cygpath`
isn't on `PATH`.

::: tip One instance at a time
The free-port picker uses `lsof`, which Git Bash doesn't ship, so on Windows it
falls back to port 4443 instead of scanning for a free one. Run a single
`just dev` at a time.
:::

::: warning "Access is denied (os error 5)" on rebuild
Stopping `just dev` can orphan the relay process, and Windows won't let `cargo`
replace a running `moq-relay.exe`. If a later build fails to remove it, kill the
leftover process and rebuild:

```bat
taskkill /IM moq-relay.exe /F
taskkill /IM moq-cli.exe /F
```

:::
