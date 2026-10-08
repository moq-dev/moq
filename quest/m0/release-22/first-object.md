# [S] Backport the clear FIRST_OBJECT fix to release

## Goal

A draft-18+ subgroup stream with FIRST_OBJECT clear whose first Object ID is
0 delivers its group on `release`, in `moq-net` and `@moq/net`, as #5027 did
on `main`. A clear bit with any other first ID is still dropped.

## Plan

Cherry-pick fdfdf9a45 (#5027) onto `release` as its own PR. `release` has
the same early drop in `rs/moq-net/src/ietf/` and in
`js/net/src/ietf/subscriber.ts` ("dropping a group with no head"). Adapt
where the two branches have diverged, and keep #5027's tests (first ID 0
delivered, first ID 3 dropped, the next group still arrives).
