# [XS] Link the benchmark trends from the docs

## Goal

A reader of the moq docs finds the [benchmark regression table](https://git.moq.dev/bench/)
and knows what the binding numbers mean when choosing a language: what each
binding costs relative to Rust, and where to see it over time.

## Plan

Decided with the maintainer on 2026-10-15 that this gets a doc page rather
than only `bench/README.md`. Follow `doc/AGENTS.md`: link to the site and
summarize, don't copy numbers that go stale. The bindings overview in
`doc/lib/` is the likely home; pick the page a language chooser reads.
