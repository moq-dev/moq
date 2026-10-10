/** Decode Rust's OBJECT_DATAGRAMs and echo JS's encoding back, plus what the JS publisher sends. */
import assert from "node:assert/strict";
import { encodeDatagram, ObjectDatagram } from "../../js/net/src/ietf/datagram.ts";
import { decodeObjectTime } from "../../js/net/src/ietf/object.ts";
import type { IetfVersion } from "../../js/net/src/ietf/version.ts";
import { Reader } from "../../js/net/src/stream.ts";
import { Timescale, Timestamp } from "../../js/net/src/time.ts";

const input: { draft: number; datagrams: number[][] } = JSON.parse(process.argv[2]);
assert(input.draft >= 14 && input.draft <= 22, `unknown draft ${input.draft}`);
const version = (0xff000000 + input.draft) as IetfVersion;

const echoes: number[][] = [];
const timestamps: (number | null)[] = [];
for (const bytes of input.datagrams) {
	const datagram = await ObjectDatagram.decode(Uint8Array.from(bytes), version);
	echoes.push([...datagram.encode(version)]);

	const properties = datagram.properties;
	const timestamp =
		properties &&
		(await new Reader(undefined, properties, version).decode((c) => decodeObjectTime(c, Timescale.MILLI)));
	timestamps.push(timestamp ? timestamp.as(Timescale.MILLI) : null);
}

// The fields of Rust's first datagram, sent the way the JS publisher sends a track's datagram.
const published = await encodeDatagram(
	{ sequence: 42, timestamp: Timestamp.fromMillis(1234), payload: new TextEncoder().encode("hello") },
	{ trackAlias: 3n, publisherPriority: 7, timescale: Timescale.MILLI },
	version,
);

// Stdout is the encoding channel back to Rust.
console.log(JSON.stringify({ echoes, timestamps, published: [...published] }));
