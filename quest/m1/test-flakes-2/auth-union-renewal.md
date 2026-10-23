# [S] A union renewal never sends a torn request update

## Goal

`js/net/src/auth.test.ts`'s "a request waits for its token and survives union
renewal" passes on every moq-transport draft every time. The subscriber never
leaves a request stream holding a partial message, so the publisher never
closes the session with "malformed request stream message" (ProtocolViolation,
code 3).

## Plan

Seen on 2026-10-22 on moqt-17 and moqt-22: about 4 failures in 120 runs on
main. The failure follows a logged `subscribe update priority=255`.

Leads from reading the code (unverified):

- `js/net/src/ietf/subscriber.ts:1098-1099` writes the message type
  (`stream.writer.u53(SubscribeUpdate.id)`) and then the body in two awaits.
  If the body's encode throws, the catch at `:1100-1103` returns silently with
  the type already sent. Teardown then FINs the stream (`:970`), and the
  publisher fails reading the size, which is exactly this error
  (`js/net/src/ietf/request_stream.ts:124`, `:312-326`).
- The test sets no priority, and wire 255 means model priority -128
  (`js/net/src/ietf/priority.ts`). The idle aggregate is 0 (wire 127), so
  either the aggregate is wrong mid-revocation or the log came from another
  session.
- `first.close()` shrinks the grant union while `#runUpdates` may be writing
  (`subscriber.ts:920-924`, `:961-973`).

Reproduce it first: loop the test until it fails, capture which session logged
255 and where the write stopped. Fix it at the source. A message goes out
whole or not at all, and an encode failure fails loudly instead of being
swallowed. Then, if the 255 is real, correct the aggregate. Add a test with a
mocked clock that tears the stream mid-update. Check the Rust subscriber for
the same split write.

The test's draft matrix runs only drafts 17 and 22, but
`SubscribeUpdate`'s encode has a separate draft 14-16 path (an extra request
ID and different fields). Add a representative draft 14-16 case, or every
supported IETF ALPN, to both the existing test and the new one, so the
legacy encoding is covered too.

Public API: none. Wire: none.
