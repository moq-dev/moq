# [S] Release-line JS keeps requests alive after FIN

## Goal

The release-line `@moq/net` publisher keeps serving a draft 19+ subscription
after the requester FINs its request stream, matching the Rust publisher
since #5133; drafts 14-18 still cancel on FIN.

## Plan

#4658 changed both languages on `main`; #5133 ported only Rust. Port the JS
publisher half to `js/net/src/ietf/` on `release`, with a version-swept test,
and bump `@moq/net` on `release` after it lands (`/bump`). Matters when a
browser publishes through a draft 19+ peer that FINs after SUBSCRIBE.
