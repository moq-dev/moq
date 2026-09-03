# [M] js/net: connect() logs on every connection at the wrong level and prints the JWT in the URL

## Goal

Implement and verify the behavior tracked in [#2405](https://github.com/moq-dev/moq/issues/2405)
within the issue's stated scope and boundaries.

## Plan

Rescoped during the 2026-08 grooming: the WebTransport line already logs at
debug level. What remains: redact the JWT from every logged URL (connect.ts
still prints the ?jwt= query, at console.warn on the WebSocket path) and
DEV-gate the remaining connect logs. Treat this as a credential-leak fix.

### Issue context

#### Summary

When embedding `@moq/net` in an application, every connection prints an unsolicited line to the browser console:

```
https://relay.example/path?jwt=<token>  connected via WebTransport
```

For a library consumer shipping MoQ inside a production app this reads as noise to their end users, and there is currently no way to silence it. Two smaller problems compound it:

1. **The log level does not match its siblings.** The successful-connect line is `console.log` and the WebSocket-fallback line is `console.warn`, while the adjacent ALPN/SETUP diagnostics are already `console.debug`. So these two lines are the only connect diagnostics that surface at the default console level.
2. **It prints the auth token.** The logged value is `url.toString()`, which includes the `?jwt=<token>` query parameter, so a live credential lands in the console (and in anything that captures console output) on every connect.

#### Location

`js/net/src/connection/connect.ts`, in `connect()` right after the transport race resolves:

```ts
if (session instanceof Session) {
    console.warn(url.toString(), "connected via WebSocket");
    websocketWon.add(url.toString());
} else {
    console.log(url.toString(), "connected via WebTransport");
}
```

#### Impact

- Consumers embedding MoQ see library log lines in their app console with no opt-out.
- The relay URL is logged verbatim, exposing the JWT in the console and any log sink.

#### Suggested direction (maintainer's call)

The core ask: a consumer running a production build should not see this line at all.

- **Gate the connect logs on dev vs production**, so they print only in development and are silent in prod builds. There is already a precedent to reuse: `js/signals/src/index.ts` defines

  ```ts
  const DEV = typeof import.meta.env !== "undefined" && import.meta.env?.MODE !== "production";
  ```

  and gates its leak-detection warnings on it. These connect logs (and the sibling ALPN/SETUP lines) could sit behind the same `DEV` check. Optionally also drop the level to `console.debug` so even in dev they stay out of the default console view.
- **Redact the URL** wherever a relay URL is logged: log `url.origin + url.pathname` (dropping `search`) so the `?jwt=` token is never printed, in any mode. This applies to the other `console.*(url...)` sites in `connect.ts` and `reload.ts` too.

A fuller future option (not required here) would be an opt-in `logging`/`verbose` flag on `ConnectProps` (which `ReloadProps` already extends) so consumers who want library logs in production can turn them back on. The two changes above are enough to fix the reported behavior.

## Closes

- [#2405](https://github.com/moq-dev/moq/issues/2405) - close this issue when the quest finishes
