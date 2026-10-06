# [S] A flate stream refuses an oversized append without ending

## Goal

`moq-flate` and `@moq/flate` `stream` mode refuse an append that cannot fit
the group budget with `GroupTooLarge` and leave the log intact, compressed or
not, as JSON streams do after #4911.

## Plan

Flate's stream mode rides one group, like JSON's, so it has the same hole.
Move the DEFLATE worst-case bound (`deflateBound`, private in each json
package after #4911) into the flate packages and have json reuse it, so the
bound and its tests live in one place per language. Mirror #4911's tests in
both compression modes.
