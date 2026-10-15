# [XS] Drafts type-check ignores ancestor tsconfigs

## Goal

`sh/drafts/check.sh` type-checks `drafts/moq-e2ee-*.ts` the same way wherever
the repository is checked out. Today, when this repository sits inside another
project whose root has a `tsconfig.json` (as a submodule does), TypeScript 7
refuses the per-file `tsc` call with TS5112 ("tsconfig.json is present but will
not be loaded if files are specified on commandline"), so the drafts check
fails there while passing in a standalone clone.

## Plan

Decided 2026-10-15: pass `--ignoreConfig` to the drafts `tsc` call, keeping
its explicit flags as the whole configuration. Rejected: a
`drafts/tsconfig.json` run with `-p`, which adds a config file for one script.
Reproduce by running the check
from a checkout nested under a directory holding a `tsconfig.json`, and confirm
the standalone run is unchanged.
