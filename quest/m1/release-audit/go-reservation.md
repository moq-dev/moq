# [S] Settle deterministic Go reservation release

## Goal

A Go encoder can relinquish a bandwidth allocation predictably while its
track remains subscribed, with an explicit and tested ownership contract.

## Plan

P2 investigation, not a proven leak or measured performance regression.
At main 1d43c096e0, `go/wrapper/session.go` says dropping a Reservation hands
room back, but exposes only Grant/Update and hides the generated handle.
Native allocation release depends on handle Drop. Go collection does not
guarantee prompt finalization. An encoder replacing a reservation on a still
subscribed track may therefore retain its old allocation and probe ceiling
until collection. This wrapper shape is inherited; main expands reservation
consumers and uses their ceilings for probe targets.

First reproduce replacement through the public Go wrapper and observe sibling
shares and probe targets without relying on forced GC. Closed tracks are
pruned, and Update(0) can relinquish the ceiling: this is not a claim of a
permanent native leak. Decide whether the existing operation supplies the
intended release contract and needs documentation, or explicit handle
disposal is required. Prefer the existing mechanism if it fully solves the
consumer's lifetime. Verify the chosen contract deterministically and report
any Go API change; no wire change is expected.
