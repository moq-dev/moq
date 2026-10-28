# [XS] The runner's JS client withdraws namespaces by reset

## Goal

Condition: [englishm/moq-interop-runner#177](https://github.com/englishm/moq-interop-runner/pull/177)
merges and the runner republishes the moq-dev-js client image. That PR,
from an outside contributor, bumps the moq-dev-js client's `@moq/net` from
0.4.4 to 0.4.6, which carries
[moq-dev/moq#6518](https://github.com/moq-dev/moq/pull/6518) and withdraws
namespaces with a reset. Check it with
`gh pr view 177 -R englishm/moq-interop-runner --json state`, then confirm
`subscribe-namespace-lifecycle` passes for moq-dev-js against Cloudflare
draft-18 and aiomoqt in the next runner nightly. Delete this quest once it
does.

## Plan

`@moq/net` 0.4.5 and earlier withdraw a PUBLISH_NAMESPACE on drafts 17+
with FIN plus STOP_SENDING, which draft-18 §3.3.2 does not allow. Both
relays end the namespace only on RESET_STREAM, so the subscriber never sees
NAMESPACE_DONE until the publisher's session closes. 0.4.6 shipped from
`release` with the backport, so no moq release is needed.

Do not open a competing runner PR. If #177 stalls, nudge it there; the
interop audit's standing approval covers PRs touching the `moq-dev-*`
builds. Its author tested only against moq-go, so if the nightly still
fails against Cloudflare draft-18 or aiomoqt, debug that relay pairing
with a client built from `main`.
