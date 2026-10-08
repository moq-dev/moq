import { expect, test } from "bun:test";
import { ProtocolViolation } from "../error.ts";
import { encodeVarint } from "../stream.ts";
import { ObjectDatagram } from "./datagram.ts";
import { type IetfVersion, Version } from "./version.ts";

const ALL: IetfVersion[] = [
	Version.DRAFT_14,
	Version.DRAFT_15,
	Version.DRAFT_16,
	Version.DRAFT_17,
	Version.DRAFT_18,
	Version.DRAFT_19,
	Version.DRAFT_20,
	Version.DRAFT_21,
	Version.DRAFT_22,
];

const LEGACY: IetfVersion[] = [Version.DRAFT_14, Version.DRAFT_15, Version.DRAFT_16];

function single(publisherPriority?: number): ObjectDatagram {
	return new ObjectDatagram({
		trackAlias: 3n,
		groupId: 42,
		publisherPriority,
		endOfGroup: true,
		properties: Uint8Array.of(0x10, 0x05),
		body: { payload: new TextEncoder().encode("hello") },
	});
}

async function rejects(bytes: Uint8Array, version: IetfVersion) {
	expect(await ObjectDatagram.decode(bytes, version).catch((err: unknown) => err)).toBeInstanceOf(ProtocolViolation);
}

test("roundtrips on every draft", async () => {
	for (const version of ALL) {
		const datagram = single(7);
		const bytes = datagram.encode(version);
		expect(bytes[0]).toBe(0x07); // properties, end of group, object 0
		expect(await ObjectDatagram.decode(bytes, version)).toEqual(datagram);
	}
});

test("a default priority needs draft-15", async () => {
	expect(() => single().encode(Version.DRAFT_14)).toThrow();
	await rejects(Uint8Array.of(0x0c, 0x01, 0x02), Version.DRAFT_14);

	const bytes = single().encode(Version.DRAFT_16);
	expect(bytes[0]).toBe(0x0f);
	expect(await ObjectDatagram.decode(bytes, Version.DRAFT_16)).toEqual(single());
});

test("an explicit object id and a status", async () => {
	const datagram = new ObjectDatagram({
		trackAlias: 1n,
		groupId: 2,
		objectId: 0,
		publisherPriority: 128,
		endOfGroup: false,
		body: { status: 0 },
	});
	for (const version of ALL) {
		const bytes = datagram.encode(version);
		expect(bytes[0]).toBe(0x20);
		expect(await ObjectDatagram.decode(bytes, version)).toEqual(datagram);
	}
});

test("rejects invalid types", async () => {
	for (const version of ALL) {
		// A status that ends the group, the reserved bit, a bit past the defined ones, and a
		// Type too wide for any of them.
		for (const kind of [0x22, 0x10, 0x40, 2 ** 40]) {
			const type = encodeVarint(kind, version);
			const bytes = new Uint8Array(type.byteLength + 4);
			bytes.set(type);
			bytes.set([0x01, 0x02, 0x03, 0x04], type.byteLength);
			await rejects(bytes, version);
		}
	}
});

test("a status with properties needs a Normal Object past draft-16", async () => {
	const datagram = new ObjectDatagram({
		trackAlias: 1n,
		groupId: 2,
		objectId: 0,
		publisherPriority: 0,
		endOfGroup: false,
		properties: Uint8Array.of(0x10, 0x05),
		body: { status: 3 },
	});
	for (const version of ALL) {
		if (LEGACY.includes(version)) {
			expect(await ObjectDatagram.decode(datagram.encode(version), version)).toEqual(datagram);
		} else {
			expect(() => datagram.encode(version)).toThrow();
			// The same bytes a legacy draft writes, which these drafts refuse.
			await rejects(datagram.encode(Version.DRAFT_16), version);
		}
	}
});

test("rejects an empty properties block, a truncated header, and bytes after a status", async () => {
	// PROPERTIES and ZERO_OBJECT_ID, alias 1, group 2, priority 0, empty block.
	await rejects(Uint8Array.of(0x05, 0x01, 0x02, 0x00, 0x00, 0x78), Version.DRAFT_16);
	// The group ID is missing.
	await rejects(Uint8Array.of(0x04, 0x01), Version.DRAFT_16);
	// A Normal status, then a stray byte.
	await rejects(Uint8Array.of(0x24, 0x01, 0x02, 0x00, 0x00, 0x00), Version.DRAFT_16);
});
