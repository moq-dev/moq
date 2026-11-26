# [XS] A bisect step counts the suspects its commit has

## Goal

A Kixel Quest bisect step of the Instructions workflow at a commit lacking one of the benchmarks it names counts the ones the commit has, and names the missing ones on stderr and in the job summary, instead of failing.
moq's `rust2` flake input is pinned to a kixel-ai/rust2 commit with `rust2-bench run --allow-missing` ([kixel-ai/rust2#1032](https://github.com/kixel-ai/rust2/pull/1032)), and `.github/workflows/codspeed.yml` passes `--allow-missing` wherever it passes `--benchmarks`, both in one commit.
A step still fails when its commit has none of the names, as rust2-bench and `sh/rs/instructions.sh` both do today.

## Plan

Decided 2026-11-25 (maintainer, `/quest-plan` in kixel-ai/rust2): ✅ an XS pin-bump quest in moq's m0, since Kixel Quest bisects moq today (rejected: drop it).

- A bisect step at a commit from before this lands runs that commit's own workflow and stays strict; the rust2 maintainer accepted that.
- Bump with `nix flake update rust2`, to kixel-ai/rust2@820f9a66 (the #1032 merge) or any later `main`. Only the `rust2` node and the inputs it follows should move in `flake.lock`.
- The Count step's `args+=(--benchmarks "$RUNNER_TEMP/benchmarks")` gains `--allow-missing`; `sh/rs/instructions.sh` passes it through to rust2-bench unchanged.
- `flake.nix` warns that a bump moving rust2's Valgrind starts new Kixel Quest series. rust2's `nixpkgs` lock did not move between the current pin (6ead6d56) and 9f69a30d, so the fingerprint should stay `x86_64-3059cd1d9e65`: confirm with `just rs instructions fingerprint` before and after, and say so in the PR.
- Validate on the real surface: `just rs instructions run --jobs 1 --benchmarks <file> --allow-missing` with one real name and one made-up name counts the real one and names the other; with only made-up names it fails.
- Mention `--allow-missing` in `bench/README.md`'s Instruction counts section, beside `--benchmarks`.

Public API: none. Wire: none.

## Related

- [Every counted bench repeats its instruction count](/quest/m0/repeatable-instruction-counts.md) - the other half of making bisect steps trustworthy
- [Benches move from Criterion to rust2-bench-harness](/quest/m9/bench-harness.md) - later bumps of the same pin, and renames `codspeed.yml`
- [kixel-ai/rust2#1041](https://github.com/kixel-ai/rust2/pull/1041) - plans moving rust2-bench to `kixel-ai/swole`; once it moves, moq's input changes repository, but this bump does not wait for it
