// Replays `rs/moq-net/fixtures/lite.json`, the moq-lite corpus moq-net replays too, so a decoder
// that drifts from the other language fails a test on both sides. `rs/moq-net/src/lite/corpus.rs`
// describes the cases.

import { expect, test } from "bun:test";
import { select } from "../connection/accept.ts";
import { Reader, UnexpectedEnd, Writer } from "../stream.ts";
import { isDecodeError } from "../util/decode_error.ts";
import {
	AnnounceInit,
	decodeAnnounceBroadcast,
	decodeAnnounceReplyMaybe,
	encodeAnnounceBroadcast,
	encodeAnnounceReply,
} from "./announce.ts";
import { ClientSetup } from "./compat.ts";
import { Group } from "./group.ts";
import { Version, versionName } from "./version.ts";

interface Case {
	name: string;
	version: string;
	message: "announce" | "announce_reply" | "announce_init" | "group" | "client_setup";
	hex: string;
	/** `announce_init`: the announce slots left, unbounded when absent. */
	budget?: number;
	/** `client_setup`: the one version the server accepts. */
	server?: string;
	expect: "decoded" | "refused" | "withdrawn";
}

const VERSIONS = new Map<string, Version>(Object.values(Version).map((v) => [versionName(v), v]));

function version(name: string): Version {
	const v = VERSIONS.get(name);
	if (v === undefined) throw new Error(`unknown moq-lite version: ${name}`);
	return v;
}

// Decode `r` the way a session does, returning how to encode what it decoded.
async function decode(c: Case, r: Reader, v: Version): Promise<((w: Writer) => Promise<void>) | "withdrawn"> {
	switch (c.message) {
		case "announce": {
			const msg = await decodeAnnounceBroadcast(r, v);
			if (msg.status === "refused") return "withdrawn";
			return (w) => encodeAnnounceBroadcast(w, msg, v);
		}
		case "announce_reply": {
			const msg = await decodeAnnounceReplyMaybe(r, v);
			if (!msg) throw new Error("no reply");
			return (w) => encodeAnnounceReply(w, msg, v);
		}
		case "announce_init": {
			const msg = await AnnounceInit.decode(r, v, c.budget ?? Number.POSITIVE_INFINITY);
			return (w) => msg.encode(w, v);
		}
		case "group": {
			const msg = await Group.decode(r, v);
			return (w) => msg.encode(w, v);
		}
		case "client_setup": {
			const msg = await ClientSetup.decode(r);
			if (c.server !== undefined) select(msg, version(c.server));
			return (w) => msg.encode(w);
		}
	}
}

// What a decode came to: our own encoding of what it decoded, as hex, or why it did not.
async function outcome(c: Case): Promise<string> {
	const v = version(c.version);
	const r = new Reader(undefined, Buffer.from(c.hex, "hex"), v);
	let encode: ((w: Writer) => Promise<void>) | "withdrawn";
	try {
		encode = await decode(c, r, v);
	} catch (err) {
		// Running out past every message body asks for more bytes, which no complete input should.
		return err instanceof UnexpectedEnd && !isDecodeError(err) ? "short" : "refused";
	}
	if (!(await r.done())) return "left bytes";
	if (encode === "withdrawn") return encode;
	return Buffer.from(await Writer.collect(v, encode)).toString("hex");
}

const cases = (await Bun.file(new URL("../../../../rs/moq-net/fixtures/lite.json", import.meta.url)).json()) as Case[];

test("the corpus has cases", () => {
	expect(cases.length).toBeGreaterThan(0);
});

for (const c of cases) {
	test(c.name, async () => {
		expect(await outcome(c)).toBe(c.expect === "decoded" ? c.hex : c.expect);
	});
}
