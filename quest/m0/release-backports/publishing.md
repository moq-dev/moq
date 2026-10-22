# [S] Backport wrapper publication repository validation

## Goal

Release's privileged Go, Swift, and Kotlin publication jobs only use a
workflow-run checkout whose repository passes the trusted upstream gate.

## Plan

Decided 2026-10-22 (maintainer): include the publishing hardening from
[moq-dev/moq#6084](https://github.com/moq-dev/moq/pull/6084) in the focused
backports, independently of its JS parsing changes.

Release 36f392f492 retains the workflow-run path without main's
head_repository.full_name check. Port the same-repository validation and
make each privileged downstream checkout depend on the validation job,
preserving the release line's existing tag and event behavior. Inspect the
whole dependency chain, including skipped and failed gates, rather than
assuming the condition on one job protects every checkout.

Validate accepted and refused event shapes without publishing artifacts or
exercising credentials, and run the workflow checks. The consumers are the
existing wrapper release jobs. Public API, wire, package versions, and the
selection of published artifacts are unchanged.
