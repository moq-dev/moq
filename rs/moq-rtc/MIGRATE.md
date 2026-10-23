# Migrating moq-rtc

How to update your code for each breaking change, newest first.

## Unreleased

### A WHIP `DELETE` lingers

After a `DELETE`, a WHIP server session's `Response::run` finishes the ingest and resolves only once the finished broadcast goes stale.
Drop the future to close the broadcast early. A peer that disconnects without one still closes it at once.

### Client dials return `Result<()>`

`Client::publish` and `Client::subscribe` return `Result<()>` again: successful negotiation starts the background media session.
Remove reads of their returned wire source, `Response::wire`, and `moq_rtc::wire`; this unused stats surface is removed.
`Response::run` still drives an accepted server session until it ends.
