# [S] JS subgroup data reaches the subscriber through imquic

## Goal

The runner's moq-dev-js `data-subgroup-basic` case passes against imquic
(draft-22, WebTransport), where today the subscriber receives no objects.
If the cause is in imquic, it is reported upstream and the cell is
classified instead.

## Plan

Found in the runs for
[englishm/moq-interop-runner#173](https://github.com/englishm/moq-interop-runner/pull/173)
on 2026-10-22. moq-dev-rs passes against imquic on the same draft, so
compare what the two clients put on the wire (transport, SUBSCRIBE
fields, stream types) before suspecting imquic.

Public API: unknown until the cause is found. Wire: none expected.
