<p align="center">
	<img height="128px" src="https://github.com/moq-dev/moq/blob/main/.github/logo.svg" alt="Media over QUIC">
</p>

# @moq/e2ee

[![npm version](https://img.shields.io/npm/v/@moq/e2ee)](https://www.npmjs.com/package/@moq/e2ee)
[![TypeScript](https://img.shields.io/badge/TypeScript-ready-blue.svg)](https://www.typescriptlang.org/)

End-to-end encryption for [Media over QUIC](https://moq.dev/) tracks, implementing [`moq-e2ee-01`](../../drafts/draft-lcurley-moq-e2ee.md).

A broadcast secret stays in this package. `@moq/net` sees only opaque physical names and ciphertext. There is no plaintext fallback.

## Quick Start

```bash
bun add @moq/e2ee
```

```ts
import { Track, Time } from "@moq/net";
import { Consumer, Credential, opaqueName, Producer } from "@moq/e2ee";

const secret = crypto.getRandomValues(new Uint8Array(32));
const credential = new Credential({
	context: "example.com/meeting-123",
	generation: 1,
	kid: 7,
	secret,
});

const name = await opaqueName(credential, "video");
const track = new Track.Producer(name);
const producer = await Producer.create({ track, credential, semanticName: "video" });

const group = producer.appendGroup();
await group.writeFrame({ payload: encodedFrame, timestamp: Time.Timestamp.now() });
group.close();

const consumer = await Consumer.create({
	track: track.subscribe(),
	credential,
	semanticName: "video",
});
const got = await consumer.nextGroup();
const frame = await got?.readFrame();
```

`secret` may also be a nonextractable WebCrypto HKDF key imported from those 32 bytes. The credential never serializes it.

## Names and catalogs

Physical track names are 22-character unpadded base64url, derived from the semantic name. Publish catalogs under those names; rendition-map keys inside the decrypted catalog are physical names too.

```ts
import { CatalogName, openCatalog, protectCatalog } from "@moq/e2ee";

const { physicalName, payload } = await protectCatalog({
	credential,
	semanticName: CatalogName.hang,
	plaintext: catalogJson,
});
const opened = await openCatalog({ credential, semanticName: CatalogName.hang, payload });
```

Compress `catalog.json.z` (or any compressed representation) *before* `protectCatalog`. Encrypting then compressing is refused by the profile: ciphertext does not compress, and the size ratio would leak.

## Bounded pump

WebCrypto AES-GCM is async. Each producer and consumer runs a FIFO pump so frames are never reordered:

- `DEFAULT_PUMP_DEPTH` (8) in-flight AEAD operations
- `DEFAULT_PUMP_QUEUE` (8) waiters behind that; a further submit throws rather than growing without bound

20 ms Opus (160-byte 64 kbps frames) encrypts in well under 1 ms on Bun's WebCrypto. Depth 8 is headroom for a ~160 ms capture burst or a browser main-thread stall, not a wire-profile parameter.

## Failure

Typed `Failure.code` values match the draft: `unsupported_profile`, `invalid_secret`, `identity`, `exhausted`, `reuse`, `oversize`, `authentication`, `duplicate`, `pinned_mismatch`.

A grouped authentication failure ends that track. A bad datagram is dropped and emitted on `Consumer.events`; the track continues. Retransmission writes stored ciphertext; encrypting different bytes at an existing identity is `reuse`. A second publisher for the same physical name in the same generation is a restart and is also `reuse`.
