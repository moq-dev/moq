# Verification receipts

One record of what was tested, on which source, in which environment, and what
is still unproven. `just check` and `just test` already define what passing
means; nothing here redefines it. These recipes run them unchanged and record
the identity of the run, so a pass stops being a pass when the thing it covered
moves.

```bash
just verify check           # `just check`, recorded
just verify test            # `just test`, recorded
just verify status          # re-grade the receipts against the tree right now
just verify pr [NUMBER]     # read-only: head, base freshness, required checks
just verify report [NUMBER] # both, as one record
```

The gap this closes: `just _changed` picks a base and counts untracked files,
while GitHub's `Check` workflow tests a pull request checkout of a different
commit. Nothing joined the two, so "it passed locally" and "CI is green" could
both be true about two different trees, and neither statement carried the base
it was measured against.

## The receipt

`just verify <lane>` writes `.verify/<lane>.json` and the run's output to
`.verify/<lane>.log`. Both are untracked: `just _changed` counts untracked
files, so a tracked receipt would change the scope it describes.

Output is merged and teed, so the log holds exactly what you saw. That also
means the command's stdout is not a terminal, and tools that decorate a tty
(cargo's progress line, colored diffs) print their plain form. Run the recipe
directly when that matters.

A receipt records the head, branch, selected base and its tip, the merge base,
a digest of the source, the dirty-file counts, the selected scope, the command,
its exit status and duration, the toolchain and lockfile identities, and the
digest and provenance of any overridden binary. Contents are hashed, never
copied, so an untracked secret cannot land in one. Hashes are git blob ids,
because git is the only hasher available everywhere this runs.

The source digest covers HEAD, the staged and unstaged diff against it, and the
content of every untracked file, which is the whole set of inputs a run could
have read. It is taken before and after the command: a tree that moved during
the run leaves `mixed` evidence, which is no evidence at all.

### Kinds

A receipt carries exactly one, and no report merges two:

| kind       | means                                                       |
| ---------- | ----------------------------------------------------------- |
| `static`   | lint and compile only; no behavior was executed              |
| `local`    | tests built from this checkout                               |
| `binary`   | an externally supplied binary was under test                 |
| `ci`       | a hosted run, read by `pr`                                   |
| `hardware` | a device this checkout cannot drive, recorded by hand        |

`RELAY_BIN` and `MOQ_BIN` let the smoke and WASM harnesses run against a
prebuilt relay or CLI. Either one relabels the run `binary` and records the
binary's digest and provenance. A binary from outside this checkout's target
directory is `external`, and evidence from it grades `exploratory`: useful for
investigating, never a merge credential.

Record what this repository cannot run itself the same way, so it grades under
the same rules:

```bash
just verify record hardware pixel-8 -- ./run-on-device.sh
```

### Verdicts

`status` re-derives the verdict every time, from the tree in front of it:

| verdict       | why                                                         |
| ------------- | ----------------------------------------------------------- |
| `pass`        | covers the current source                                    |
| `fail`        | the command exited nonzero                                   |
| `mixed`       | the source changed while the command ran                     |
| `stale`       | HEAD, the working tree, or the target base moved after it    |
| `exploratory` | an externally supplied binary was under test                 |
| `unreadable`  | the receipt file is truncated or corrupt                     |

Only `pass` exits zero. `status` compares the base against the ref as this
checkout last fetched it; `pr` asks GitHub, which is the answer that matters
before a merge.

## The pull request

`just verify pr` reads and never writes: no reruns, no merges, no labels. It
fetches the live head, compares it with the base branch, reads the branch's
*effective* rules, and grades every required context against the check runs and
commit statuses for that head.

A required result is `pass` only when GitHub says `success`. `missing`,
`pending`, `skipped`, and `unknown` (a neutral or absent conclusion) are each
reported as themselves, because a required job that never ran looks exactly like
a green one in a summary that only counts failures. Cancelled and timed-out runs
are failures. A rerun repeats a job name, so the newest attempt is the one that
counts. A rule may also pin the app that has to report a context, and then a
same-named run from anything else is not that required result.

A feed that could not be read is not an empty feed. Unreadable rules, check
runs, commit statuses, or base comparison each make the report `incomplete`,
because "nothing failed" and "nothing was read" print the same way otherwise.

Last, GitHub's own `mergeStateStatus` has to agree. Anything but `CLEAN` or
`HAS_HOOKS` is `incomplete`: something is gating the merge that this report
cannot see, and a green verdict would be claiming otherwise.

The report's verdict is `failed`, `incomplete`, `pending`, `stale`, or `green`,
and only `green` exits zero. `stale` is the interesting one: it means every
result is green but something has moved on since, either the head is behind its
base or the local receipts describe a different commit.

Having no local receipt at all is not staleness. A required result that passed
on this exact head is stronger evidence than a local run, so the record says it
is hosted-only and stays green. A receipt describing a *different* head is
staleness, because it invites a reader to credit this candidate with what
another one proved.

`mergeable` comes back `UNKNOWN` on the first request for a pull request GitHub
has not compared recently; it computes the answer in the background and returns
it to the next request. That reads as `pending` here rather than as a pass, and
asking again resolves it.

## Merging is a separate action

A report is evidence, not an authorization token. Nothing here merges, and the
verdict is not a permission bit: it is what was true when it was printed. The
merge itself has to recheck the head against GitHub's own gate, which is what
`gh pr merge --squash --match-head-commit <sha>` does. Only GitHub can hold that
gate against a target branch that moves between the check and the merge.

## Branch policy

The `main` (2420853) and `dev` (13599435) rulesets, read on 2026-09-07 UTC,
require `Check` and `Test`, with `strict_required_status_checks_policy: false`
and no merge-queue rule in either. The classic branch-protection endpoint
answers "Branch not protected" for both, which is what a ruleset-only repository
looks like; read `/repos/{owner}/{repo}/rules/branches/{branch}` instead, as
`pr` does.

So nothing requires a pull request to be tested against the current tip of its
base. That is the freshness gap this reports as `stale`, and it is a real one:
`Check` is scoped to the diff against the base, so a change that lands on `main`
after a run is neither in the tested tree nor in its selected scope.

Two ways to close it, neither adopted here, because both are the maintainer's
call and both cost CI time on every merge:

- **A merge queue.** GitHub builds the candidate on top of the current base and
  merges only if it passes. It needs the workflows to also trigger on
  `merge_group`, and `just _changed` to resolve the queue's base and head from
  that event rather than from `GITHUB_BASE_REF`, which a `merge_group` payload
  does not set.
- **Strict required status checks.** One flag, no workflow changes, but every
  pull request has to be updated whenever its base moves, which serializes
  merges by hand.

The read-only report is deliberately the first step: it makes the gap visible on
the candidates where it matters before anything starts re-running CI for it.
