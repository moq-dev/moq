# [XS] Our runner cases land upstream

## Goal

[englishm/moq-interop-runner#172](https://github.com/englishm/moq-interop-runner/pull/172)
(moq-dev-rs `data-subgroup-basic`) and
[englishm/moq-interop-runner#173](https://github.com/englishm/moq-interop-runner/pull/173)
(moq-dev-js `subscribe-namespace-lifecycle` and `data-subgroup-basic`)
are merged, the moq-dev client images are rebuilt, and the runner's report
shows both clients passing those cases against moq-relay. Check the PRs
with `gh pr view -R englishm/moq-interop-runner` and the cells on the
nightly report. Then delete this quest.

## Plan

Both PRs were tested on 2026-10-22 against moq-relay from `main` and from
the 0.17.7 release, on drafts 14 through 22 and on moq-lite, and both pass.
The report runs the published packages and images, so it turns green
only after the PRs merge and the client images are rebuilt. The rebuild is
already requested in
[englishm/moq-interop-runner#170](https://github.com/englishm/moq-interop-runner/pull/170).

Why the report showed moq-relay failing `data-subgroup-basic` against
Cloudflare's moq-test-client (draft-18):

- Ours: every forwarded object carried an extra property, the Timestamp
  that released relays stamp on every object, even on a track that
  declared no timescale. Fixed on `main` by
  [moq-dev/moq#5083](https://github.com/moq-dev/moq/pull/5083), and it
  turns green once a relay release ships it.
- Theirs: the publisher writes its first object while it answers the
  relay's SUBSCRIBE, so SUBSCRIBE_OK's Largest Location is (0, 0), and it
  refuses the relay's joining FETCH (REQUEST_ERROR 3). moq-relay joins the
  live edge with Largest Object plus a joining FETCH (draft-18 §5.1.3),
  which is the draft's own join, so object 0 is lost. When the group's
  stream arrives before the refusal, all of group 0 is lost.
- Disputed: the client fails on any status object other than End of Group.
  moq-relay ends the track with END_OF_TRACK at (3, 0), which draft-18
  allows (§11.2.1.1). The runner's data-plane conventions leave status
  objects out of verification.

Both are reported upstream, in
[cloudflare/moq-rs#281](https://github.com/cloudflare/moq-rs/issues/281)
and [cloudflare/moq-rs#282](https://github.com/cloudflare/moq-rs/issues/282).
moq-relay keeps its END_OF_TRACK (decided 2026-10-22). The group that
moq-relay drops when the joining FETCH is refused is
[headless moq-transport groups](/quest/m2/ietf-headless-group.md).

The moq-dev-rs `subscribe-namespace-lifecycle` timeouts in the report come
from the stale client image. Rebuilt from the runner's current source, it
passes against moxygen and Cloudflare draft-18. Cloudflare draft-14 has no
SUBSCRIBE_NAMESPACE, so that cell is unsupported.

Two moq-dev-js cells from the runs for #173 still fail, with causes not
yet found:
[JS namespace lifecycle in one process](/quest/m1/interop-coverage/js-namespace-done.md)
and [JS subgroup data through imquic](/quest/m1/interop-coverage/js-imquic-objects.md).

Public API: none. Wire: none.
