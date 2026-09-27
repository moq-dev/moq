# [L] Group FETCH

## Goal

An IETF FETCH that asks for whole groups is answered. A group already in
cache is served from there. A miss fetches that group upstream. Subscribers
still FETCH one group at a time. A publisher may be asked for a range, which
the relay walks one group at a time. A publisher refusal is what the
subscriber sees.

## Plan

Standalone FETCH and a non-zero joining FETCH are refused today with
"not supported". Walk `track::Consumer::fetch_group`, one group, then the
next. Do not add an archive.

A joining FETCH is the same walk for the groups it names. A form the walk
cannot express is still an explicit refusal, not a hang.

The moxygen FETCH cases that ask for whole groups are the check. The rest of
that suite is not.

## Related

- [Moxygen compatibility](/quest/m1/moxygen/README.md) - the line this belongs to
- [JavaScript FETCH](/quest/m1/js-fetch.md) - the browser publisher that fills an upstream miss
