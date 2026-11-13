# [XS] moq-boy is on nix.moq.dev

## Goal

Every `main` commit whose Cache run succeeds has `moq-boy` for x86_64-linux on
nix.moq.dev, beside moq-relay and moq-cli, so a deploy that pins such a commit
(`just gh cache-rev`) downloads `github:moq-dev/moq/<rev>#moq-boy` instead of
compiling it.

## Plan

Decided 2026-11-13 (maintainer, /quest-run follow-up): ✅ plan it in m1,
since the demo deploys count as urgent (rejected: leave it; m3). The consumers are the demo deploys on the maintainer's host, which
pin a moq commit and compile moq-boy on each deploy today. Their pins are that
host's own work, not this repository's.

- What the Cache workflow pushes is chosen in `sh/gh/cache-runtime.sh`, which
  `cache.yml`'s `runtime` job runs through `just gh cache-runtime`. It pushes
  `.#moq-relay.cargoArtifacts` and `.#moq.cargoArtifacts`, then `.#moq-relay`
  and `.#moq`, records their roots for the commit, and refreshes them inside
  the bucket's 14-day expiry. The `warm` matrix pushes only dev shells.
- Add `.#moq-boy.cargoArtifacts` and `.#moq-boy` to those two builds. Its
  features (`openh264`) and inputs (ffmpeg) differ from the relay's and CLI's,
  so crane builds it its own dependency artifacts; pushing them lets the next
  run substitute them, as it does for the other two.
- Rename what the job, its justfile recipe, and the script's comments say they
  publish: "Smoke's runtime packages" becomes the packages Smoke and deploys
  pin.
- Cost: `nix copy` uploads whole closures, and moq-boy's adds ffmpeg's
  (`ffmpeg-9.0.2-lib`, 305 MiB uncompressed per cache.nixos.org), which changes
  only when nixpkgs moves. The runtime job also builds moq-boy cold whenever its
  dependency artifacts miss, under the job's 75-minute limit. Report the added
  job time and upload from the first `main` run after merging.
- x86_64-linux only, the one platform the `runtime` job builds. Another
  platform waits for a consumer on it.
- Check after the merge: once the merge commit's Cache run succeeds,
  `nix build github:moq-dev/moq/<rev>#moq-boy` on a machine without it
  substitutes rather than printing `building ...moq-boy`.
