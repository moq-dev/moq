# Migrating moq-stats

How to update your code for each breaking change, newest first.

## Unreleased

### A returning group restarts under a new epoch (#4904)

A stats group that returns announces a new epoch and starts its group numbers at zero, instead of continuing the old sequence.
A reader that keyed history on the path alone should end that history when the announcement ends and start fresh on the next one.
