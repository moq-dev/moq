# [S] Every auth mode is a client of Admissions

## Goal

`moq_relay::Auth` has one admission path. `admit()` is send-plus-await on
the `Admissions` queue. `--auth-url`, `--auth-public`, and refuse are
decider tasks that answer that queue; embedded is the same queue with no
task, so the embedder answers it. A change to how a session is admitted,
re-checked, or ended is made once.

`--auth-url` / `--auth-public` / refuse / embedded stay as configuration.
`Mode` is gone. Gateways still do not call `admit`. `admit_fixed` stays the
LAN bypass and is not a fifth mode. Not this quest: the lease clock, in-band
tokens, or a live stats retier.

## Plan

Fold Server, Public, and Refuse onto the queue Embedded already uses.

- `Auth` always holds the `Admission` sender. `admit()` sends and awaits the
  oneshot, the way Embedded does today, including the ten-second bound.
- `Config::init` builds that queue and spawns the matching decider, so it
  needs a Tokio runtime. `a_public_config_admits_anonymous_and_certificate_alike`
  becomes a `#[tokio::test]`. Empty config (no url, no public) still means
  embedded: `Relay::load` returns `Admissions` and spawns nothing.
- `--auth-url`: a loop on `Admissions::next` that `tokio::spawn`s one task
  per admission, `Client::connect(request)`, then `grant` or `refuse`. A slow
  server does not serialize connects.
- `--auth-public`: a loop that answers `grant(lease::Consumer::fixed(grant))`.
- Refuse (`Auth::refuse`, LAN-only with no listener): a loop that answers
  `auth::Error::Refused`. A dropped `Admissions` stays an outage (502), not a
  policy. `Auth::refuse` also requires a Tokio runtime; cover synchronous
  construction with the same runtime-requirement tests as `Config::init`.
- Dropping every `Auth` clone ends `next()` with `None` and the decider
  exits. Dropping the decider first makes later `admit()` fail unavailable,
  the same as dropping `Admissions` today.
- `Mode` is deleted. The server path is tested through the same grant/refuse
  answers the embedded tests already use.

How a downstream embedder uses the landed API, for reference. moq.pro's edge
serves the contract on a unix socket today and points `--auth-url` at itself;
with `Admissions` its `auth::start` takes `relay.admissions()` after
`Relay::load` and runs one loop:

```rust
while let Some(admission) = admissions.next().await {
    let authorizer = authorizer.clone();
    tokio::spawn(async move {
        match authorizer.decide(Facts::from_request(&admission.request), false).await {
            Ok(grant) if grant.revalidate.is_none() => admission.grant(lease::Consumer::fixed(grant)),
            Ok(grant) => {
                let (producer, consumer) = lease::Producer::new(grant.clone());
                admission.grant(consumer);
                authorizer.drive(producer, admission.request, grant).await;
            }
            Err(Error::Refused(why)) => admission.refuse(auth::Error::Refused),
            Err(Error::Unavailable(why)) => admission.refuse(auth::Error::Unavailable(why)),
        }
    });
}
```

`drive` is the edge's re-check loop: sleep the cadence, `decide(.., true)`,
`producer.update` or `producer.revoke`, and `producer.closed()` for the end
event with the totals the session reported through `lease::Consumer::close`.
The unix socket, the axum router, and the JSON round trip go away; the
gateways keep not calling `admit`, and relay/CLI `--listen` keep calling it.

Public API: breaking on moq-relay's unpublished auth module (`Mode` gone,
`Config::init` and `Auth::refuse` require a runtime). Wire: none.

## Related

- [Auth embedder](/quest/m2/auth-embedder.md) - the lease owns the re-check clock, so `drive` above becomes a loop over `producer.due()` instead of a second driver
- [In-band auth](/quest/m2/auth/README.md) - a token presented in band is
  another admission on the same lease
- [Stats retier](/quest/m2/stats-retier.md) - what a re-checked grant should
  do to a live session, whichever path re-checked it
