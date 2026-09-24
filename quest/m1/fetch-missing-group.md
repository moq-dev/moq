# [S] Fetching a missing group fails before any output

## Goal

HTTP `/fetch/<broadcast>/<track>?group=N` answers 404 for a group the track
does not have, and `moq fetch --group N` exits with a clean "not found"
before writing anything. Today the lookup hands back a group consumer that
only fails on its first frame read, so the endpoint answers 200 with a
cut-off body.

## Plan

- Resolve whether the group exists before the response starts, in the
  shared lookup the relay and the CLI both use. Where the check belongs (the
  relay helper or a `moq-net` track consumer method) is the implementer's
  call; propose it in the PR.
- Test: a missing group is a 404 over HTTP and a non-zero `moq fetch` exit
  with no stdout; an existing group is byte-identical to today.

## Required

- `moq fetch` (#3965) has merged
