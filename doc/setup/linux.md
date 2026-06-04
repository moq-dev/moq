---
title: Linux Installation
description: Install moq-relay, moq-cli, and the moq-gst plugin on Linux
---

# Linux Installation

MoQ ships native Linux packages for Debian/Ubuntu and Fedora/RHEL/openSUSE.
The relay, the CLI, the token utility, and the GStreamer plugin all install
via your system package manager and stay current through normal
`apt upgrade` / `dnf upgrade`.

## Debian and Ubuntu

Tested on Debian 12 (bookworm), Debian 13 (trixie), Ubuntu 22.04, Ubuntu 24.04.
The `gstreamer1.0-moq` plugin needs GStreamer >= 1.22 and is only available
on Debian 12+ / Ubuntu 24.04+; the other packages install on Ubuntu 22.04 too.

```bash
# Trust the project's signing key.
curl -fsSL https://apt.moq.dev/moq-archive-keyring.gpg \
  | sudo tee /usr/share/keyrings/moq-archive-keyring.gpg > /dev/null

# Add the repository.
echo "deb [signed-by=/usr/share/keyrings/moq-archive-keyring.gpg] https://apt.moq.dev stable main" \
  | sudo tee /etc/apt/sources.list.d/moq.list

sudo apt update

# Pick what you need.
sudo apt install moq-relay        # relay server with systemd unit
sudo apt install moq-cli          # publish/subscribe CLI
sudo apt install moq-token-cli    # JWT token utility for moq-relay
sudo apt install gstreamer1.0-moq # GStreamer plugin (moqsink, moqsrc)
```

## Fedora, RHEL, Rocky, AlmaLinux, openSUSE

Tested on Fedora 39+, RHEL 9, Rocky 9, AlmaLinux 9.

```bash
sudo dnf config-manager --add-repo https://rpm.moq.dev/moq.repo
sudo dnf install moq-relay         # relay server with systemd unit
sudo dnf install moq-cli           # publish/subscribe CLI
sudo dnf install moq-token-cli     # JWT token utility
sudo dnf install gstreamer1-moq    # GStreamer plugin
```

On openSUSE use `zypper addrepo` instead of `dnf config-manager`.

## Running moq-relay

The package drops a systemd unit and a default config at
`/etc/moq-relay/relay.toml`. Place your TLS cert, key, and JWK
under `/var/lib/moq-relay/` (the service's `StateDirectory`), edit the
config to taste, and enable the service:

```bash
sudo install -d -m 0750 /var/lib/moq-relay
sudo cp cert.pem key.pem root.jwk /var/lib/moq-relay/
sudo systemctl enable --now moq-relay
sudo journalctl -u moq-relay -f
```

The service runs as a `DynamicUser` with `CAP_NET_BIND_SERVICE`, so port
443 works without root. Your edits to `/etc/moq-relay/relay.toml` survive
package upgrades.

## Other Linux distributions

If your distro doesn't have a native package on offer:

- **Alpine, NixOS, air-gapped systems**: download the static binary from the
  [GitHub Releases](https://github.com/moq-dev/moq/releases) page. Each
  release attaches `moq-relay-v<ver>-x86_64-unknown-linux-gnu` and
  `aarch64-unknown-linux-gnu` variants.
- **Docker**: `docker pull docker.io/moqdev/moq-relay:latest`. Multi-arch
  images for `linux/amd64` and `linux/arm64`.
- **Nix**: the project ships a flake. The Cachix cache at `kixelated.cachix.org`
  serves pre-built binaries, but only tagged releases are pushed, so pin the
  ref to a recent tag and accept the flake's cache config to skip building from
  source:

  ```bash
  nix run github:moq-dev/moq/moq-relay-v0.12.4#moq-relay --accept-flake-config
  ```

  An unpinned `github:moq-dev/moq#moq-relay` tracks the default branch, which
  is not cached and compiles from source. To trust the cache permanently
  instead of per-command, run `cachix use kixelated` once.
- **Arch Linux**: a community-maintained PKGBUILD lives in the AUR
  (`moq-relay-bin`). The project doesn't maintain it directly; treat it as
  community supported.
- **From source**: any system with a Rust toolchain can build via
  `cargo install moq-relay`. The relay has no external runtime dependencies
  beyond glibc.

## Verifying signatures

The apt and rpm repositories are signed with the same project GPG key. The
public key is served at:

- <https://apt.moq.dev/moq-archive-keyring.gpg>
- <https://rpm.moq.dev/moq-archive-keyring.gpg>

Verify the apt repository metadata signature manually:

```bash
gpg --import moq-archive-keyring.gpg
gpg --verify Release.gpg Release
```
