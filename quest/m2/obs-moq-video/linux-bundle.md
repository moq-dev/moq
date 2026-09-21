# [M] Ship a prebuilt Linux obs-moq bundle

## Goal

Every `obs-moq-v*` release attaches a Linux x86_64 tarball that loads into a stock distro's OBS 32+, so Linux users install the plugin the same way macOS and Windows users do instead of building from source. `doc/bin/obs.md` and the moq.pro OBS guide drop their Linux build-from-source recipe.

## Plan

- The only reason `obs-build` in `.github/workflows/libmoq.yml` skips Linux is FFmpeg: the source links nix/distro libavcodec, which is not portable. The FFmpeg removal is the blocker; once the plugin is C++ over libmoq plus libobs and Qt6, a Linux build has no extra runtime dependency that OBS itself does not already carry.
- Build on `ubuntu-24.04` (glibc 2.39, the floor OBS's own Linux packages target) against the libobs and Qt6 headers OBS's plugin template uses; the template's `.deb` recipe is the reference. Ship the plain archive layout the other platforms use (`obs-moq-*-x86_64-unknown-linux-gnu.tar.gz` with `bin/64bit/obs-moq.so` and `data/`), extractable into `~/.config/obs-studio/plugins/obs-moq/`. A `.deb` is optional and separate.
- Flatpak OBS cannot load a plugin from the host filesystem. Verify a real Flatpak install and support it only if the sandbox's runtime ABI matches, documenting the `~/.var/app/com.obsproject.Studio/config/obs-studio/plugins/` path. Otherwise, state plainly that Flatpak is unsupported.
- `cpp/obs/build.sh --target x86_64-unknown-linux-gnu` produces the tarball, and the matrix in `obs-build` gains the row; nothing else in the release pipeline changes. Keep the nightly `just obs ci` compile as the PR gate.
- Verify by loading the tarball into the oldest supported OBS 32 release and current stable on Ubuntu 24.04 and one non-Debian distro (Fedora), publishing and subscribing against a relay, and inspecting the `.so` with `ldd` for nothing beyond libobs, Qt6, glibc, and OS libraries.

## Required

- [Video source replacement](/quest/m2/obs-moq-video/source.md) - removes the FFmpeg linkage that makes a Linux binary non-portable

## Related

- [Linux decoded frames](/quest/m2/obs-moq-video/decode-linux.md) - native surface delivery lands on top of the portable CPU path this bundle ships
