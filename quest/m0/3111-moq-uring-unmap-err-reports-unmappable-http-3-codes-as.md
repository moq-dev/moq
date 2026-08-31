# [M] moq-uring: unmap_err reports unmappable HTTP/3 codes as MoQ application errors

## Goal

Implement and verify the behavior tracked in [#3111](https://github.com/moq-dev/moq/issues/3111)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

`unmap_err` in `rs/moq-uring/src/quic/web.rs` falls back to the original code when `error_from_http3` returns `None`, but keeps the variant it came in as:

```rust
fn unmap_err(err: Error) -> Error {
	let unmap = |code: u64| proto::error_from_http3(code).map(u64::from).unwrap_or(code);
	match err {
		Error::Reset(code) => Error::Reset(unmap(code)),
		Error::Stop(code) => Error::Stop(unmap(code)),
		Error::App { code, reason } => Error::App { code: unmap(code), reason },
		other => other,
	}
}
```

`App`, `Reset`, and `Stop` are exactly the variants that `web_transport_trait::Error` reports through its accessors:

```rust
fn session_error(&self) -> Option<(u32, String)> {
	match self {
		Self::App { code, reason } => Some((u32::try_from(*code).unwrap_or(u32::MAX), reason.clone())),
		_ => None,
	}
}

fn stream_error(&self) -> Option<u32> {
	match self {
		Self::Reset(code) | Self::Stop(code) => Some(u32::try_from(*code).unwrap_or(u32::MAX)),
		_ => None,
	}
}
```

So an HTTP/3 *connection* error such as `H3_NO_ERROR` (0x100) reaches `moq_net::Error::from_transport` looking like an application code the peer chose, and a RESET\_STREAM code with no WebTransport preimage looks like a valid MoQ stream error. That conflates "HTTP/3 failed" with "the peer sent you this WebTransport code", which is the distinction the mapping exists to preserve.

#### Shape

`Error` already has a `Transport { code, reason }` variant whose accessors both return `None`, which is the honest landing spot for a code that has no WebTransport meaning. Making an unmapped code fall into a transport-flavored variant is a contract change rather than a local fix, which is why it was left out of #3081.

Worth settling before the layer ships: once a consumer is reading these codes, changing which variant an unmappable code arrives as is a behavior break.

Split out of #3096 (finding 1), which is otherwise covered by #3103, #3105, and #3106. Raised by Codex reviewing #3081 and verified against `dev`.

## Closes

- [#3111](https://github.com/moq-dev/moq/issues/3111) - close this issue when the quest finishes
