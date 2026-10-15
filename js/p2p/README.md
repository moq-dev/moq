# @moq/p2p

A WebTransport-shaped MoQ transport over one reliable ordered WebRTC data channel.
Applications own signaling, ICE configuration, admission, and the `RTCPeerConnection`.
The transport owns its data channel. Closing it leaves the peer connection open.

```ts
import { Transport } from "@moq/p2p";
import { Connection } from "@moq/net";

// The application agrees this ALPN with its peer through signaling.
const protocol = "moq-lite-07-wip";
const transport = Transport.connect({ peer, protocol });
const connection = await Connection.connect({ url, transport });
```

The receiver adopts the incoming channel with
`Transport.accept({ peer, channel, protocol })` and passes that transport to
`Connection.accept({ url, transport })`. The channel's protocol must match the
agreed ALPN. Unordered or partially reliable channels are refused.

Each binary data-channel message carries one qmux-01 record without its size
field. Records are capped at 16 KiB and the negotiated SCTP message limit;
a limit below qmux-01's 16,382-byte minimum is refused. There are no datagrams.
SCTP preserves ordering, so packet loss can hold back every stream.

`just test p2p` runs SETUP and a media transfer between two real peer connections
in Chromium. `just js p2p-bench` sweeps stream counts and payload sizes through
an in-memory data-channel pair.
