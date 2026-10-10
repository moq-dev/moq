# [S] Interop close-code test sees the refusal it expects

## Goal

The Interop browser close-code test (`test/interop/clients/js/close.ts`)
reliably reports a refused session as `unauthorized`, never as
"Connection lost", fixed at its cause.

## Plan

Seen 2026-10-10 on #5158's first Interop run (`refused session`: "Connection
lost" instead of "unauthorized"); it passed on rerun and the PR didn't touch
that code. Planned under this questline's cause-first rules (decided
2026-10-10): find whether the refusal's close code is lost on the wire (a
close frame racing the connection teardown, as in the qmux close-flush work)
or the client reads the connection's state before the close arrives. Fix
that at its source, with a regression that fails without it. No retry and no
accepting either message.

Public API: none expected. Wire: none expected.
