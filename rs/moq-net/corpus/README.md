# Corpus

Wire inputs with the outcome both implementations must reach. `rs/moq-net`
(`lite::corpus`) and `js/net` (`src/lite/corpus.test.ts`) replay every case under
`just check`, so a decoder that drifts from the other language fails on both sides.

## Cases

One JSON file per case in `lite/`, named for what it pins:

```json
{
	"version": "moq-lite-02",
	"message": "announce_init",
	"budget": 1,
	"hex": "050201610162",
	"expect": "refused"
}
```

- `version`: the negotiated version, by its handshake name (`moq-lite-01` to `moq-lite-07-wip`).
- `message`: what the bytes are, length prefix and any type included:
  - `announce`: one announcement on the announce stream.
  - `announce_init`: a lite-01/02 ANNOUNCE\_INIT. `budget` is the announce slots left, unbounded when absent.
  - `group`: a group stream header.
  - `client_setup`: a `moql` CLIENT\_SETUP. `server` is the one version the server accepts, and refusing to pick it refuses the case.
- `hex`: the input.
- `expect`: `decoded`, where our own encoding of the decoded value must equal `hex`, so a case
  also pins the value, or `refused`, where the input is rejected without asking for more bytes.

Every input is complete: asking for more bytes fails either expectation.

A fuzz crash on a moq-lite message (`fuzz/regressions/`) belongs here instead, with
`expect` set, so `js/net` replays it too.
