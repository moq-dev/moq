# [XS] A CRLF checkout installs and builds

## Goal

A Windows or WSL clone made with `core.autocrlf=true` gets LF text files, so
`just install` builds `@moq/libav` through Nix, substitutes it from
`nix.moq.dev` instead of compiling it, and the shell scripts and justfiles
run.

## Plan

Reported in [moq-dev/moq#7957](https://github.com/moq-dev/moq/issues/7957):
on WSL, the `moq` postinstall fails in the libav.js patchPhase with
`$'\r': command not found`. `.gitattributes` has no repository-wide eol rule,
so autocrlf gives `js/libav/package.nix` CRLF; its multi-line `postPatch`
carries a `\r` on every line, and the changed text also changes the derivation
hash, so the cache misses and ffmpeg builds from source.

Add `* text=auto eol=lf` to `.gitattributes` and renormalize
(`git add --renormalize .`), checking that no binary file is misdetected as
text. Add a WSL note to `doc/setup/dev.md` saying a checkout made before the
rule needs a fresh clone.

Decided 2026-11-05 (maintainer, /quest-import):

- ✅ `js/libav/build.ts` stays Nix-first. Once CRLF stops changing the hash,
  the `nix.moq.dev` cache ([moq-dev/moq#7902](https://github.com/moq-dev/moq/pull/7902)) makes that a download, and npm-first would
  need a guard against serving a stale npm build after a `package.nix` edit.
- ✅ Installing the private `moq` monorepo as a git dependency stays
  unsupported: its workspaces aren't linked there, and Nix resolves `../..`
  against the outer repository. A clone or submodule works once this lands
  (with Nix) or `@moq/libav` 0.1.0 is on npm (without Nix). Not chosen: a
  preview-package workflow (pkg.pr.new) for fork authors, which the reporter
  didn't ask for.

Public API: none. Wire: none.

## Closes

- [#7957](https://github.com/moq-dev/moq/issues/7957) - libav install and build fail on WSL

## Related

- [Claim the new @moq npm names](/quest/m2/release/npm-first-publish.md) - publishes `@moq/libav`, which Nix-less installs need
