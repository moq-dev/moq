# [S] A retracted broadcast still releases its tracks' demand

## Goal

Once a broadcast is unannounced and the retraction has settled, dropping the
last subscriber of one of its tracks resolves `track.demand().unused()` on the
publisher, as it does before the retraction and on `release`. Today the demand
stays used forever while the producer lives.

In a relay this keeps every demand-driven emit loop (stats, overlays) and its
upstream subscription running after an unannounce, for nobody. moq.pro's
billing-meter test `a_reannounced_node_is_read_once` fails on `main` because of
it. `release` doesn't have the regression, so this lands before the next
release cut.

## Plan

Regression from [#4741](https://github.com/moq-dev/moq/pull/4741): the repro
below passes on its parent (16b1fe2da) and on `release` (3492aebf4), and fails
at 0382d309f and on `main` 2704e10e2.
The unsettled case passes, so the hold comes from the origin processing the
retraction while the reader is still subscribed.

Suspect, unverified: a front's `Action::End` in `rs/moq-net/src/model/origin.rs`
keeps a used track's copy in flight ("readers follow the copy they read to its
end"). The copy then outlives its readers and keeps the source track
subscribed until the publisher ends it, which a live producer never does. A
copy kept in flight after a retraction should still be released when its last
reader leaves.

Land the repro as the regression test (moq-tokio, paused time; `control` and
`unannounced` pass on `main`, `unannounced_settled` hits the 600 s virtual
timeout):

```rust
use std::time::Duration;

async fn case(unannounce: bool, settle: bool) {
	let origin = moq_tokio::origin::spawn();
	let broadcast = origin.publish("a/b", moq_net::origin::Route::default()).unwrap();
	let track = broadcast.create_track("t", None).unwrap();
	let consumer = origin.consume().request_broadcast("a/b").await.unwrap();
	let sub = consumer.track("t").unwrap().subscribe(None).await.unwrap();
	track.demand().used().await.unwrap();
	if unannounce {
		broadcast.unannounce();
		if settle {
			tokio::time::sleep(Duration::from_secs(1)).await;
		}
	}
	drop(sub);
	drop(consumer);
	let demand = track.demand();
	let unused = demand.unused();
	tokio::time::timeout(Duration::from_secs(600), unused).await.expect("released").unwrap();
}

#[tokio::test(start_paused = true)]
async fn control() { case(false, false).await }
#[tokio::test(start_paused = true)]
async fn unannounced() { case(true, false).await }
#[tokio::test(start_paused = true)]
async fn unannounced_settled() { case(true, true).await }
```

Also cover a remote source, the case a relay hits (`rs/moq-net/tests`, the mock
harness, every version): publisher origin P announces the broadcast, relay R
pulls it over `connect_mock`, and a reader subscribes to the track on R. P
unannounces and the retraction settles. Drop R's reader, then assert P's
`track.demand().unused()` resolves, which shows R's upstream SUBSCRIBE was
cancelled. If
[Request linger](/quest/m1/request-linger.md) lands first, the release waits
out its linger, not forever.

## Related

- [Demand everywhere](/quest/m1/demand-everywhere.md) - the `demand()` handles this watches
- [Request linger](/quest/m1/request-linger.md) - bounds how long an abandoned upstream request may outlive its readers
- [Broadcast epochs](/quest/m0/broadcast-epoch/README.md) - gates the release #4741 ships in
