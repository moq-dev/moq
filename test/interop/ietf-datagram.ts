/** Decode Rust's OBJECT_DATAGRAMs and echo JS's encoding back, plus what the JS publisher sends. */
import assert from "node:assert/strict";
import { encodeDatagram, ObjectDatagram } from "../../js/net/src/ietf/datagram.ts";
import type { IetfVersion } from "../../js/net/src/ietf/version.ts";
import { Timestamp } from "../../js/net/src/time.ts";

const input: { draft: number; datagrams: number[][] } = JSON.parse(process.argv[2]);
assert(input.draft >= 14 && input.draft <= 22, `unknown draft ${input.draft}`);
const version = (0xff000000 + input.draft) as IetfVersion;

const echoes: number[][] = [];
for (const bytes of input.datagrams) {
	const datagram = await ObjectDatagram.decode(Uint8Array.from(bytes), version);
	echoes.push([...datagram.encode(version)]);
}

// The fields of Rust's first datagram, sent the way the JS publisher sends a track's datagram:
// without properties, its timestamp included.
const published = await encodeDatagram(
	{ sequence: 42n, timestamp: Timestamp.fromMillis(1234), payload: new TextEncoder().encode("hello") },
	{ trackAlias: 3n, publisherPriority: 7 },
	version,
);

// Stdout is the encoding channel back to Rust.
console.log(JSON.stringify({ echoes, published: [...published] }));
