# Risk

`quest review` reads this file from the base to give each PR a risk tier,
low, medium, or high, from its diff alone. Changing this file, a workflow
under `.github/workflows/`, or an action under `.github/actions/` is
always high: `quest` holds those itself, and the `## High` list below adds
the paths whose every change is high.

A merge to `main` publishes nothing: packages ship only when a release cut
merges `main` into `release`, as [RELEASE.md](RELEASE.md) describes. A bad
merge reaches users through the next cut, and until then costs the
contributors and agents building on `main`. Moq.pro runs relays from
releases, not from `main`.

## Tiers

High is a change that breaks what a published consumer or a deployed peer
relies on: an API break or a wire change, as
[Releases](RELEASE.md#releases) defines them. That covers the public Rust,
JS, and bindings API, a frozen wire version, the hang catalog JSON, a
stored or exported container format, and CLI flags, config keys, and
environment variables. A change to an unpublished `-wip` wire version
counts, since the next cut publishes it. Also high:

- A change to what a token grants or how a relay checks it (`rs/moq-auth`,
  `js/auth`, and the relay's use of them).
- A change to when an agent may push, merge, post, or spend without the
  maintainer, such as `CONTRIBUTING.md`'s landing gate or a skill that
  merges.
- A change to the scripts a release workflow runs to package or publish an
  artifact, such as `packaging/` and the `package` and `publish` scripts
  under `sh/`.

Medium is a change to shipped code that keeps its API and wire format,
such as a fix or an additive API, which the next cut publishes as a patch.
Also a change to what the merge gate selects (`sh/dispatch.sh`,
`sh/changed.sh`), since a selection that misses a package lets a break
reach `main`, and a change to agent instructions that keeps what agents may
do on their own.

Low is a change to tests, benchmarks, TLA+ models, quest plans, examples,
demos, or documentation, which nothing a consumer or peer relies on reads;
a revert fully undoes it.

## High

- `RELEASE.md` - agents follow it to cut a release or backport a fix
- `version.json` - its phase decides whether `main` may take API breaks
- `.release-plz.toml` - it decides which crates release-plz versions and
  publishes
- `sh/version.py` - it sets and checks every package's version at a cut
- `sh/api-diff/` - it refuses a published API break on a backport and while
  the phase is additive
- `sh/gh/release.sh` - every release workflow reads tags and versions
  through it
