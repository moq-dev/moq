# Migrating moq-stats

How to update your code for each breaking change, newest first.

## Unreleased

### The `.z` flavor ends with the plain one past 32 MB

A frame whose uncompressed JSON passes the 32 MB group budget now ends both the plain track and its `.z` sibling with `FrameTooLarge`: a reader inflates the whole group, so compression no longer carries a larger frame.
A reader that fell back to `.z` for very large frames sees the same end the plain reader does.

### A returning group restarts under a new epoch (#4904)

A stats group that returns announces a new epoch and starts its group numbers at zero, instead of continuing the old sequence.
A reader that keyed history on the path alone should end that history when the announcement ends and start fresh on the next one.
