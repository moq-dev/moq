# [XS] FLV rebind before header

## Goal

A single-track FLV export whose first catalog snapshot lacks the best
rendition switches to it if it appears before the stream header goes out, so
the pick doesn't depend on catalog timing.

## Plan

`flv::Export` binds from the first snapshot and ignores later ones in
single-track mode. Before the header, a better-ranked rendition could replace
the bound one; after it, FLV can't introduce a new config, so the current track
stays. Mock time in the test.
