# [S] hang applies catalog updates at their timestamp

## Goal

A hang catalog update takes effect at its timestamp on the broadcast clock,
not when a consumer reads it. `drafts/draft-lcurley-moq-hang.md` says so, and
`rs/hang` and `js/hang` hand each update to the application with its
timestamp, so a rendition added or removed, or a change in a section such as
a composition's, lines up with the frame stamped alike.

## Plan

Decided 2026-11-12 (maintainer, moq-dev/studio /quest-plan): ✅ catalogs are
timed from lite-07 on, and every update applies at its timestamp, as one rule
for the whole catalog (rejected: timing only a composition section, which
leaves two timing rules in one catalog). Its first consumer is
[the composition](/quest/m3/layers/spec.md). It lands in m2, beside lite-07
(rejected: m3 with the composition), so lite-07 ships with the rule.

[The model](/quest/m2/time-addressing/model.md) already has moq-mux's catalog
producer stamp each catalog from the broadcast clock. Today
`rs/hang/src/catalog/consumer.rs` drops that stamp (`.map(|value|
value.value)`), and `js/hang` does likewise; both return it with the
catalog. A consumer that reads an update ahead of its timestamp holds it
until media reaches that time; one that reads it late applies it at once.

A scale-less broadcast's catalog has no media time to wait for, so it applies
on arrival, as today.

Decided 2026-11-14 (the m2 triage before the cut, under /quest-afk, for the
maintainer's review): ✅ this waits only for the Rust model, not for
[@moq/net by start](/quest/m2/time-addressing/js.md) (rejected: keeping
that wait). A catalog's stamp is its frame's timestamp, which `@moq/net`
frames already carry (moq-json's `Timed.at`), and nothing here reads a group
start. The JS half checks that `js/hang`'s catalog producer stamps from the
broadcast clock, and makes it do so if not. It stays in m2: `rs/hang` and
`js/hang` return the stamp with each catalog, a breaking change to both
consumers, and the hang draft's rule is what lite-07 catalogs mean.

## Required

- [Rust groups are addressed by their start](/quest/m2/time-addressing/model.md) - the catalog producer stamps from the broadcast clock
