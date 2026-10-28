// Every vector in drafts/moq-e2ee-00.json, run against the internal primitives.
import { expect, test } from "bun:test";
import { Epoch } from "@moq/net";
import { Credential } from "./credential.ts";
import { type Domain, decode, encode, nameKeyInfo, nameMacInfo, nonce, pad, pathInfo } from "./derive.ts";
import { Failure } from "./failure.ts";
import { keyBytes, TrackKey, trackKey } from "./key.ts";
import * as Limits from "./limits.ts";
import { NameKeys } from "./name.ts";
import { expand } from "./secret.ts";

type Row = Record<string, unknown>;

const vectors = (await Bun.file(new URL("../../../drafts/moq-e2ee-00.json", import.meta.url)).json()) as {
	profile: string;
	constants: Row;
	generation: Row;
	paths: Row[];
	derivation: Row[];
	naming: Row[];
	groups: Row[];
	datagrams: Row[];
	negative: Row[];
};

const encoder = new TextEncoder();

function unhex(value: unknown): Uint8Array<ArrayBuffer> {
	const text = String(value ?? "");
	const out = new Uint8Array(text.length / 2);
	for (let i = 0; i < out.length; i++) out[i] = Number.parseInt(text.slice(i * 2, i * 2 + 2), 16);
	return out;
}

function hex(bytes: Uint8Array): string {
	return Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
}

function code(fn: () => unknown): string {
	try {
		fn();
	} catch (err) {
		if (err instanceof Failure) return err.code;
		throw err;
	}
	throw new Error("expected a failure");
}

async function rejection(promise: Promise<unknown>): Promise<string> {
	try {
		await promise;
	} catch (err) {
		if (err instanceof Failure) return err.code;
		throw err;
	}
	throw new Error("expected a failure");
}

function credentialFrom(row: Row): Credential {
	return new Credential({ context: unhex(row.context), kid: row.kid as number, secret: unhex(row.secret) });
}

// Epoch text the net layer refuses is `identity` at the profile boundary.
function epochFrom(row: Row): Epoch.Valid {
	try {
		return Epoch.parse(String(row.epoch));
	} catch {
		throw new Failure("identity");
	}
}

function generationFrom(row: Row) {
	const credential = credentialFrom(row);
	return credential.consume(epochFrom(row));
}

// Group inputs are decimal strings; frame inputs are numbers or non-finite names.
function identity(value: unknown): number {
	return typeof value === "string" ? Number(value) : (value as number);
}

test("constants match the vectors", () => {
	const c = vectors.constants;
	expect(vectors.profile).toBe(Limits.PROFILE);
	expect(c.salt).toBe(hex(encoder.encode(Limits.PROFILE)));
	expect(c.path_label).toBe(hex(encoder.encode(Limits.PATH_LABEL)));
	expect(c.name_mac_label).toBe(hex(encoder.encode(Limits.NAME_MAC_LABEL)));
	expect(c.name_key_label).toBe(hex(encoder.encode(Limits.NAME_KEY_LABEL)));
	expect(c.key_label).toBe(hex(encoder.encode(Limits.KEY_LABEL)));
	expect(c.domain_group).toBe(0);
	expect(c.domain_datagram).toBe(1);
	expect(c.tag_len).toBe(Limits.TAG_LEN);
	expect(c.key_len).toBe(Limits.KEY_LEN);
	expect(c.path_len).toBe(Limits.PATH_LEN);
	expect(c.name_mac_len).toBe(Limits.NAME_MAC_LEN);
	expect(c.name_nonce_len).toBe(Limits.NAME_NONCE_LEN);
	expect(c.name_block).toBe(Limits.NAME_BLOCK);
	expect(c.max_u53).toBe(Limits.MAX_U53);
	expect(c.max_u32).toBe(Limits.MAX_U32);
	expect(c.max_invocations).toBe(Limits.MAX_INVOCATIONS);
	expect(c.max_plaintext_bytes).toBe(Limits.MAX_PLAINTEXT_BYTES);
	expect(c.max_grouped_payload).toBe(Limits.MAX_GROUPED_PAYLOAD);
	expect(c.max_grouped_plaintext).toBe(Limits.MAX_GROUPED_PLAINTEXT);
	expect(c.max_datagram_body).toBe(Limits.MAX_DATAGRAM_BODY);
	expect(c.max_datagram_header).toBe(Limits.MAX_DATAGRAM_HEADER);
	expect(c.max_datagram_payload).toBe(Limits.MAX_DATAGRAM_PAYLOAD);
	expect(c.max_datagram_plaintext).toBe(Limits.MAX_DATAGRAM_PLAINTEXT);
});

test("path vectors", async () => {
	for (const row of vectors.paths) {
		const credential = credentialFrom(row);
		const semantic = String(row.semantic_name);
		expect(hex(encoder.encode(semantic))).toBe(String(row.semantic_name_bytes));
		expect(hex(pathInfo(credential.context, credential.kid, semantic))).toBe(String(row.path_info));
		const path = await credential.path(semantic);
		expect(path).toBe(String(row.opaque) as typeof path);
		expect(encode(unhex(row.path_material))).toBe(String(row.opaque));
	}
});

test("derivation vectors", async () => {
	for (const row of vectors.derivation) {
		const generation = generationFrom(row);
		const { credential, epoch } = generation;
		const semantic = String(row.semantic_name);
		expect(hex(encoder.encode(semantic))).toBe(String(row.semantic_name_bytes));
		const macInfo = nameMacInfo(credential.context, epoch, credential.kid);
		expect(hex(macInfo)).toBe(String(row.name_mac_info));
		expect(hex(await expand(credential, macInfo, Limits.NAME_MAC_LEN))).toBe(String(row.name_mac));
		const keyInfo = nameKeyInfo(credential.context, epoch, credential.kid);
		expect(hex(keyInfo)).toBe(String(row.name_key_info));
		expect(hex(await expand(credential, keyInfo))).toBe(String(row.name_key));
		expect(hex(pad(semantic))).toBe(String(row.padded));
		const name = await generation.name(semantic);
		expect(name).toBe(String(row.physical_name) as typeof name);
		expect(hex(decode(name).subarray(0, Limits.NAME_NONCE_LEN))).toBe(String(row.name_nonce));
		const key = await keyBytes(credential, epoch, semantic, row.domain as Domain);
		expect(hex(key)).toBe(String(row.key));
	}
});

test("naming vectors", async () => {
	const credential = credentialFrom(vectors.generation);
	for (const row of vectors.naming) {
		const semantic = String(row.semantic_name);
		expect(hex(encoder.encode(semantic))).toBe(String(row.semantic_name_bytes));
		expect(hex(pad(semantic))).toBe(String(row.padded));
		const epoch = epochFrom(row);
		const name = await credential.consume(epoch).name(semantic);
		expect(name).toBe(String(row.physical_name) as typeof name);
		// A producing generation under this epoch recovers the semantic name from the physical one alone.
		expect(await new NameKeys(credential, epoch).open(String(row.physical_name))).toBe(semantic);
	}
});

async function payloadRow(row: Row) {
	const generation = generationFrom(row);
	const semantic = String(row.semantic_name);
	const name = await generation.name(semantic);
	expect(name).toBe(String(row.physical_name) as typeof name);
	const domain = row.domain as Domain;
	expect(hex(await keyBytes(generation.credential, generation.epoch, semantic, domain))).toBe(String(row.key));
	const group = row.group as number;
	const frame = row.frame as number;
	expect(hex(nonce(group, frame))).toBe(String(row.nonce));

	const plaintext = unhex(row.plaintext);
	const limit = row.payload_limit as number;
	const key = trackKey(generation.credential, generation.epoch, semantic, domain);
	const payload = await key.encrypt(key.admitSeal(group, frame, plaintext.byteLength, limit), plaintext);
	expect(hex(payload)).toBe(String(row.payload));
	const opened = await key.decrypt(key.admitOpen(group, frame, payload.byteLength, limit), payload);
	expect(hex(opened)).toBe(String(row.plaintext));
}

test("group vectors", async () => {
	for (const row of vectors.groups) await payloadRow(row);
});

test("datagram vectors", async () => {
	for (const row of vectors.datagrams) {
		expect(row.payload_limit).toBe(Limits.MAX_DATAGRAM_PAYLOAD);
		await payloadRow(row);
	}
});

test("negative vectors", async () => {
	const generation = vectors.generation;
	const credential = credentialFrom(generation);
	for (const row of vectors.negative) {
		let got: string;
		switch (row.operation) {
			case "open": {
				const source = generationFrom(row.generation as Row);
				const semantic = String(row.semantic_name);
				const domain = row.domain as Domain;
				expect(hex(await keyBytes(source.credential, source.epoch, semantic, domain))).toBe(String(row.key));
				const key = trackKey(source.credential, source.epoch, semantic, domain);
				const payload = unhex(row.payload);
				const group = row.group as number;
				const frame = row.frame as number;
				got = await rejection(
					(async () =>
						key.decrypt(
							key.admitOpen(group, frame, payload.byteLength, row.payload_limit as number),
							payload,
						))(),
				);
				break;
			}
			case "identity":
				got = code(() => nonce(identity(row.group), identity(row.frame)));
				break;
			case "generation":
				got = code(() => generationFrom(row.generation as Row));
				break;
			case "name": {
				const source = row.generation as Row;
				const names = new NameKeys(credentialFrom(source), epochFrom(source));
				got = await rejection(names.open(String(row.physical_name)));
				break;
			}
			case "bytes": {
				const data = "a".repeat(row.length as number);
				switch (row.field) {
					case "context":
						got = code(
							() =>
								new Credential({
									context: encoder.encode(data),
									kid: 7,
									secret: unhex(generation.secret),
								}),
						);
						break;
					case "epoch":
						got = code(() => credential.consume(data as Epoch.Valid));
						break;
					case "semantic_name":
						got = await rejection(credential.produce().name(data));
						expect(await rejection(credential.path(data))).toBe(got);
						break;
					default:
						throw new Error(`unknown bytes field ${row.field}`);
				}
				break;
			}
			case "protect": {
				const key = new TrackKey(Promise.reject(new Error("never derived")));
				const length = row.plaintext_len as number;
				got = code(() =>
					key.admitSeal(row.group as number, row.frame as number, length, row.payload_limit as number),
				);
				expect(key.invocations).toBe(0);
				break;
			}
			default:
				throw new Error(`unknown operation ${row.operation}`);
		}
		expect(`${row.id}: ${got}`).toBe(`${row.id}: ${row.error}`);
	}
});
