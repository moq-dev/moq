# [XS] A gate host is rented

## Goal

Condition: the maintainer picks a host for the public repositories' merge
gates and rents it, with root SSH reachable for a NixOS install. Check with
`ssh root@<host> true`. Delete this quest once it passes.

## Plan

Shelved 2026-10-26 by the maintainer after the comparison in
[the gate host quest](/quest/m3/ci-host.md). Recommendation: netcup RS 4000
G12.5, one month first, then a 12-month term once entry times match the home
host. Before ordering, re-measure the gate's busy hours if `quest land` has
shipped, since batching may change the answer (see that quest's SaaS row).
