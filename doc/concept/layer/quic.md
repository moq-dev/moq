---
title: QUIC
description: RFC9000 Baybe
---

# QUIC
[RFC9000](https://datatracker.ietf.org/doc/html/rfc9000) - QUIC: A UDP-Based Multiplexed and Secure Transport

## Reliability
QUIC provides various forms of reliability:
- **Full Reliability**: A QUIC stream will be retransmitted until every byte arrives.
- **Partial Reliability**: A QUIC stream can be immediately RESET with an error code, aborting any forward progress.
- **No Reliability**: A QUIC datagram (extension) can be sent and will not be queued/retransmitted.

## Decoupling
QUIC crucially decouples the delivery of streams.
QUIC streams do not block each other, unlike some other protocols that appear to offer concurrency (ex. SCTP, HTTP/2).
The sender can prioritize streams by deciding which packet to send next.
Either side can reset a stream to abruptly terminate it.
