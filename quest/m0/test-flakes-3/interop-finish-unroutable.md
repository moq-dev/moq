# [M] A request right after its announcement is not refused unroutable

## Goal

`just test interop --all` passes its `finish` lane under load. A relay never
answers UNROUTABLE to a request for a claim it announced milliseconds
earlier, and a deterministic test (a kio simulation or a relay integration
test) fails without the fix.

## Plan

Seen 2026-11-17 on
[moq-dev/moq#8987](https://github.com/moq-dev/moq/pull/8987): 3 of about 30
runs of the lane, all while the machine was loaded by other builds, and none
in 19 runs once the load fell or under 8 busy loops. Whether `main` before
that PR does the same was not checked.

The lane's publisher ends its broadcast when the linger runs out and
announces `<broadcast>.health` on the same lite-07 session. Every reader
waits for the first to go and subscribes to `live` on the second the moment
it is announced. In each failing run the relay logged, in order: its session
with the publisher announcing the health route
(`lite::subscriber::routed: announce`), then about 5 ms later
`subscribed error ... err=unroutable` for the first three or four readers,
then about 8 ms later `source closed` for the ended broadcast on the
publisher's session, then the upstream subscribe starting for the readers
that asked after that, which all pass. The readers refused never retry, so
they pass their deadline.

Ruled out with tracing: `route::node::State::forward` returned no refusal in
a failing run, so the node found the claim's slot serving and the refusal
came from the origin beneath it. The sites left are the ones answering
`Error::Unroutable` in `rs/moq-net/src/model/origin.rs` (`Requesting::poll_ok`
when its lookup fails while discovery is synced, the lookup's own
`best_route` under the node filter, and the pinned instance check) and the
validity checks in `rs/moq-net/src/model/broadcast.rs`. Tracing them did not
catch a failure, since it stopped reproducing.

Do not fix it in the test with a retry: a viewer that subscribes as a
broadcast appears would see the same refusal.

Public API: none. Wire: none expected.

## Related

- [The lite-07 route hang](/quest/m0/nightly/lite07-route-hang.md) - another timing fault on the lite-07 route path, with a bench that reproduces
- [What the route switch left](/quest/m2/multicast/switch.md) - the PR that saw it
