# [XS] Data tracks stamp on the catalog's current clock

## Goal

A JSON or binary data track publishes timestamps on the same clock mapping as
the media next to it, however early it was created. Today a data track copies
the catalog clock when it's created. An importer sets that clock from its
first frame (remove-live, #4543), so a data track created before that frame
maps its timestamps to a different clock than the media.

## Plan

Remove live() (#4543) is done on `dev`, and the first-frame clock anchor
exists only there, so this targets `dev`.

Have data tracks read the catalog's clock when they stamp, not a copy made
when they were created. Keep it crate-private if possible. Test: a data track
created before an importer's first frame stamps on the anchored clock.
