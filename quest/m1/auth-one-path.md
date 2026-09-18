# [S] Every auth mode is a client of Admissions

## Goal

`moq_relay::Auth` has one admission path. Today it has four: `Server` asks
`moq_auth::Client`, `Public` hands out a fixed lease, `Refuse` says no, and
`Embedded` queues an `Admission` for whoever holds the `Admissions`. The
embedder's path is the general one: a `connect` request goes out, a `Lease`
or an `auth::Error` comes back, and a `lease::Producer` somewhere drives the
session for as long as it runs. The other three could be tasks answering the
same queue, so a change to how a session is admitted, re-checked, or ended is
made once.

## Plan

Unplanned. The shape landed with the embedded mode, so this quest is the
decision whether to fold the rest onto it:

- `auth::Config::init` would spawn the decider: for `--auth-url`, a task that
  takes each `Admission`, calls `Client::connect(request)`, and answers
  `grant(consumer)` or `refuse(err.into())`, one spawned task per admission so
  a slow server does not serialize connects; for `--auth-public`, a task
  answering `grant(lease::Consumer::fixed(grant))`. `Refuse` stays a mode, or becomes a
  task answering `auth::Error::Refused`: a dropped `Admissions` is an outage
  (502), not a policy.
- The costs to weigh: one channel hop and one task per admission on the
  server path; `init` needing a Tokio runtime, which
  `a_public_config_admits_anonymous_and_certificate_alike` today proves it
  does not; and `Auth` no longer being self-contained, so a clone kept past
  the decider task admits nothing.
- The upside: `Mode` disappears, `admit` is `send` plus `await`, and the
  server path is tested through the same `Decider` the embedded tests use.

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
event with the totals the session reported through `lease::Consumer::close`. The unix socket, the axum router, and the JSON
round trip go away; the gateways keep calling `auth.admit` and now reach the
same loop.

## Related

- [Auth embedder](/quest/m2/auth-embedder.md) - the lease owns the re-check clock, so `drive` above becomes a loop over `producer.due()` instead of a second driver
- [In-band auth](/quest/m2/auth/README.md) - a token presented in band is
  another admission on the same lease
- [Stats retier](/quest/m2/stats-retier.md) - what a re-checked grant should
  do to a live session, whichever path re-checked it
