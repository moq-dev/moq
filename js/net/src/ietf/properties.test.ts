import { expect, test } from "bun:test";
import { concat, encodeVarint, Reader } from "../stream.ts";
import { Timescale } from "../time.ts";
import { decode } from "./properties.ts";
import { Version } from "./version.ts";

const version = Version.DRAFT_23;
const varint = (v: number | bigint) => encodeVarint(v, version);

/** ID_TIMESCALE sealed in Immutable Properties picks the timestamp Object ID scheme, as a mutable one does. */
test("draft-23: a sealed ID_TIMESCALE is read, and a second copy is malformed", async () => {
	const block = concat([varint(0x40b70), varint(1000)]);
	const sealed = concat([varint(0x0b), varint(block.byteLength), block]);
	const parse = (bytes: Uint8Array) => decode(new Reader(undefined, bytes, version), version);

	expect((await parse(sealed)).timescale).toBe(Timescale(1000));
	// Delta-encoded: ID_TIMESCALE (0x40B70) after the block (0x0b).
	const twice = concat([sealed, varint(0x40b70 - 0x0b), varint(1000)]);
	await expect(parse(twice)).rejects.toThrow("repeated property");
});
