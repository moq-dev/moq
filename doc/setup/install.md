---
title: Install
description: Install moq-relay, moq-cli, moq-token, and the plugins on Linux, macOS, and Windows
---

# Install

Three binaries and two plugins ship prebuilt:

| Package | Binary | What it does |
| --- | --- | --- |
| `moq-relay` | `moq-relay` | The [relay server](/bin/relay/) |
| `moq-cli` | `moq` | The [media router](/bin/cli): publish, play, convert, gateways, and `moq token` |
| `moq-token-cli` | `moq-token` | Standalone JWT tool; the same commands as `moq token` |
| GStreamer plugin | `moqsink`, `moqsrc` | [GStreamer](/bin/gstreamer) elements |
| OBS plugin | | [OBS Studio](/bin/obs) output and source |

## Any platform

```bash
# crates.io (needs a Rust toolchain)
cargo install moq-relay moq-cli moq-token-cli

# Homebrew (macOS and Linux)
brew install moq-dev/tap/moq-relay moq-dev/tap/moq-cli

# Nix (pin a release tag to use the binary cache)
nix run github:moq-dev/moq#moq-relay -- relay.toml
nix run github:moq-dev/moq#moq-cli -- --help

# Docker (linux/amd64 and linux/arm64)
docker run -p 4443:4443/udp -p 4443:4443/tcp -v "$PWD/relay.toml:/app/relay.toml:ro" moqdev/moq-relay /app/relay.toml
docker run -i moqdev/moq-cli --help
```

Static binaries for Linux (x86\_64, aarch64), macOS (Apple Silicon), and Windows
(x64) are attached to every
[GitHub release](https://github.com/moq-dev/moq/releases). The Nix cache at
`kixelated.cachix.org` only holds tagged releases, so an unpinned
`github:moq-dev/moq` builds from source.

## Debian and Ubuntu

Debian 12+ and Ubuntu 22.04+. The GStreamer plugin needs GStreamer 1.22, so it
is available on Debian 12+ and Ubuntu 24.04+.

```bash
curl -fsSL https://apt.moq.dev/moq-keyring.gpg | sudo tee /usr/share/keyrings/moq-keyring.gpg > /dev/null
echo "deb [signed-by=/usr/share/keyrings/moq-keyring.gpg] https://apt.moq.dev stable main" | sudo tee /etc/apt/sources.list.d/moq.list
sudo apt update
sudo apt install moq-relay moq-cli moq-token-cli gstreamer1.0-moq
```

## Fedora, RHEL, and openSUSE

Fedora 39+, RHEL 9, Rocky 9, AlmaLinux 9. On openSUSE use `zypper addrepo`.

```bash
sudo dnf config-manager --add-repo https://rpm.moq.dev/moq.repo
sudo dnf install moq-relay moq-cli moq-token-cli gstreamer1-moq
```

Both repositories are signed with the project key, served at
`https://apt.moq.dev/moq-keyring.gpg` and `https://rpm.moq.dev/moq-keyring.gpg`.

### Running the relay as a service

The Linux packages install a systemd unit and a default config at
`/etc/moq-relay/relay.toml`. Put the certificate, key, and JWK under
`/var/lib/moq-relay/`, then:

```bash
sudo systemctl enable --now moq-relay
sudo journalctl -u moq-relay -f
```

The service runs as a dynamic user with `CAP_NET_BIND_SERVICE`, so port 443
works without root, and config edits survive upgrades.

## Windows

```powershell
winget install moq-dev.moq-relay
winget install moq-dev.moq-cli
winget install moq-dev.moq-token-cli
```

The OBS plugin ships as a zip for Windows x64 and macOS arm64 on the
[`obs-moq` releases](https://github.com/moq-dev/moq/releases?q=obs-moq); see
[OBS](/bin/obs). To build the repository itself on Windows, see
[Development](/setup/dev#windows).

## Other

- **Arch Linux**: a community-maintained `moq-relay-bin` PKGBUILD lives in the AUR.
- **Air-gapped hosts**: use the release binaries. They link glibc 2.34+, so Alpine and other musl distributions should use the Docker image or build from source.
- **From source**: `cargo build --release -p moq-relay` (or `-p moq-cli`) in a checkout.
