# [S] Cluster drill with a flapping peer link

## Goal

The two-relay burst drill also runs with the peer link flapping mid-burst,
and every group still arrives or fails by name within its bound. This is the
regression for the route-flap drops the 0.15.6 tree showed in #4349, which
current `main` no longer reproduces in a mock.

## Plan

Extend `bursts_cross_a_cluster` (#4920, `rs/moq-relay/tests/drills.rs`) rather
than adding a harness: flap the shaped peer link during a burst, keep strict
grading, and add a drill mutation proving the flap is graded. A moq-transport
peer link variant is optional; add it only if it is cheap here.
