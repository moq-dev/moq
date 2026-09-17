#!/usr/bin/env bun
/**
 * moq-e2ee-00 primitives and known-answer vectors.
 *
 * `bun drafts/moq-e2ee-00.ts` verifies drafts/moq-e2ee-00.json against this
 * file. `bun drafts/moq-e2ee-00.ts --write` regenerates the JSON.
 *
 * This is the language-neutral contract, not the TypeScript E2EE core: it does
 * not wrap tracks, groups, or datagrams.
 */

import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

export const PROFILE = "moq-e2ee-00";
export const SALT = utf8("moq-e2ee-00");
export const NAME_LABEL = utf8("moq-e2ee-00 name");
export const KEY_LABEL = utf8("moq-e2ee-00 key");
export const DOMAIN_GROUP = 0x00;
export const DOMAIN_DATAGRAM = 0x01;
export const TAG_LEN = 16;
export const KEY_LEN = 16;
export const NAME_LEN = 16;
export const HASH_LEN = 32;
export const MAX_U53 = Number.MAX_SAFE_INTEGER;
export const MAX_U32 = 2 ** 32 - 1;
export const MAX_INVOCATIONS = 2 ** 24;
export const MAX_PLAINTEXT_BYTES = 2 ** 36;
export const MAX_GROUPED_PAYLOAD = 32 * 1024 * 1024;
export const MAX_GROUPED_PLAINTEXT = MAX_GROUPED_PAYLOAD - TAG_LEN;
export const MAX_DATAGRAM_BODY = 1200;
// Three QUIC varints at their widest: the publisher cannot see the Subscribe ID each hop encodes.
export const MAX_DATAGRAM_HEADER = 24;
export const MAX_DATAGRAM_PAYLOAD = MAX_DATAGRAM_BODY - MAX_DATAGRAM_HEADER;
export const MAX_DATAGRAM_PLAINTEXT = MAX_DATAGRAM_PAYLOAD - TAG_LEN;

export type Domain = typeof DOMAIN_GROUP | typeof DOMAIN_DATAGRAM;

export type Credential = {
	context: Uint8Array<ArrayBuffer>;
	kid: bigint;
	secret: Uint8Array<ArrayBuffer>;
};

/** One credential under one publisher-minted epoch: the scope of every derivation. */
export type Generation = Credential & { epoch: Uint8Array<ArrayBuffer> };

export class ProfileError extends Error {
	readonly code: string;
	constructor(code: string, message = code) {
		super(message);
		this.code = code;
	}
}

export function utf8(value: string): Uint8Array<ArrayBuffer> {
	return new TextEncoder().encode(value);
}

export function hex(bytes: Uint8Array<ArrayBuffer>): string {
	return [...bytes].map((b) => b.toString(16).padStart(2, "0")).join("");
}

export function unhex(value: string): Uint8Array<ArrayBuffer> {
	if (value.length % 2 !== 0) throw new Error(`odd hex length: ${value.length}`);
	const out = new Uint8Array(value.length / 2);
	for (let i = 0; i < out.length; i++) {
		out[i] = Number.parseInt(value.slice(i * 2, i * 2 + 2), 16);
	}
	return out;
}

export function concat(...parts: Uint8Array<ArrayBuffer>[]): Uint8Array<ArrayBuffer> {
	const out = new Uint8Array(parts.reduce((n, p) => n + p.length, 0));
	let offset = 0;
	for (const part of parts) {
		out.set(part, offset);
		offset += part.length;
	}
	return out;
}

export function encodeU16(value: number): Uint8Array<ArrayBuffer> {
	if (!Number.isInteger(value) || value < 0 || value > 0xffff)
		throw new ProfileError("identity", `u16 out of range: ${value}`);
	return new Uint8Array([(value >> 8) & 0xff, value & 0xff]);
}

export function encodeU32(value: number): Uint8Array<ArrayBuffer> {
	if (!Number.isInteger(value) || value < 0 || value > MAX_U32)
		throw new ProfileError("identity", `u32 out of range: ${value}`);
	const out = new Uint8Array(4);
	new DataView(out.buffer).setUint32(0, value);
	return out;
}

export function encodeU64(value: bigint): Uint8Array<ArrayBuffer> {
	if (value < 0n || value > 0xffffffffffffffffn) {
		throw new ProfileError("identity", `u64 out of range: ${value}`);
	}
	const out = new Uint8Array(8);
	new DataView(out.buffer).setBigUint64(0, value);
	return out;
}

export function encodeBytes(value: Uint8Array<ArrayBuffer>): Uint8Array<ArrayBuffer> {
	if (value.length > 0xffff) throw new ProfileError("identity", `bytes too long: ${value.length}`);
	return concat(encodeU16(value.length), value);
}

export function base64url(bytes: Uint8Array<ArrayBuffer>): string {
	return btoa(String.fromCharCode(...bytes))
		.replaceAll("+", "-")
		.replaceAll("/", "_")
		.replaceAll("=", "");
}

export function checkCredential(credential: Credential): void {
	if (credential.secret.length !== 32) throw new ProfileError("invalid_secret");
	if (credential.context.length > 0xffff) throw new ProfileError("identity", "context exceeds 65535 bytes");
	checkU53(credential.kid, "kid");
}

export function checkGeneration(generation: Generation): void {
	checkCredential(generation);
	if (generation.epoch.length === 0) throw new ProfileError("identity", "epoch is empty");
	if (generation.epoch.length > 0xffff) throw new ProfileError("identity", "epoch exceeds 65535 bytes");
	if (generation.epoch.includes(0x2f)) throw new ProfileError("identity", "epoch contains /");
}

function checkU53(value: bigint, label: string): void {
	if (value < 0n || value > BigInt(MAX_U53)) {
		throw new ProfileError("identity", `${label} exceeds MAX_SAFE_INTEGER`);
	}
}

export function checkPhysicalName(name: string): void {
	if (name.length !== 22 || !/^[A-Za-z0-9_-]{22}$/.test(name)) {
		throw new ProfileError("identity", `malformed physical name: ${name}`);
	}
}

export function checkIdentity(group: bigint, frame: number): void {
	checkU53(group, "group");
	if (!Number.isInteger(frame) || frame < 0 || frame > MAX_U32)
		throw new ProfileError("identity", "frame exceeds 32 bits");
}

export function nonce(group: bigint, frame: number): Uint8Array<ArrayBuffer> {
	checkIdentity(group, frame);
	return concat(encodeU64(group), encodeU32(frame));
}

async function hmacSha256(
	key: Uint8Array<ArrayBuffer>,
	data: Uint8Array<ArrayBuffer>,
): Promise<Uint8Array<ArrayBuffer>> {
	const cryptoKey = await crypto.subtle.importKey("raw", key, { name: "HMAC", hash: "SHA-256" }, false, ["sign"]);
	return new Uint8Array(await crypto.subtle.sign("HMAC", cryptoKey, data));
}

export async function extract(credential: Credential, salt = SALT): Promise<Uint8Array<ArrayBuffer>> {
	checkCredential(credential);
	return hmacSha256(salt, credential.secret);
}

async function hkdfExpand(
	prk: Uint8Array<ArrayBuffer>,
	info: Uint8Array<ArrayBuffer>,
	length: number,
): Promise<Uint8Array<ArrayBuffer>> {
	const n = Math.ceil(length / HASH_LEN);
	if (n > 255) throw new Error("HKDF expand too long");
	const out = new Uint8Array(n * HASH_LEN);
	let previous = new Uint8Array(0);
	for (let i = 1; i <= n; i++) {
		previous = await hmacSha256(prk, concat(previous, info, new Uint8Array([i])));
		out.set(previous, (i - 1) * HASH_LEN);
	}
	return out.slice(0, length);
}

export function nameInfo(generation: Generation, semanticName: Uint8Array<ArrayBuffer>): Uint8Array<ArrayBuffer> {
	return concat(
		NAME_LABEL,
		encodeBytes(generation.context),
		encodeBytes(generation.epoch),
		encodeU64(generation.kid),
		encodeBytes(semanticName),
	);
}

export function keyInfo(generation: Generation, physicalName: string, domain: Domain): Uint8Array<ArrayBuffer> {
	return concat(
		KEY_LABEL,
		encodeBytes(generation.context),
		encodeBytes(generation.epoch),
		encodeU64(generation.kid),
		encodeBytes(utf8(physicalName)),
		new Uint8Array([domain]),
	);
}

export async function deriveName(
	credential: Generation,
	semanticName: Uint8Array<ArrayBuffer>,
	prk?: Uint8Array<ArrayBuffer>,
): Promise<{
	prk: Uint8Array<ArrayBuffer>;
	info: Uint8Array<ArrayBuffer>;
	material: Uint8Array<ArrayBuffer>;
	physicalName: string;
}> {
	checkGeneration(credential);
	const extracted = prk ?? (await extract(credential));
	const info = nameInfo(credential, semanticName);
	const material = await hkdfExpand(extracted, info, NAME_LEN);
	return { prk: extracted, info, material, physicalName: base64url(material) };
}

export async function deriveKey(
	credential: Generation,
	physicalName: string,
	domain: Domain,
	prk?: Uint8Array<ArrayBuffer>,
): Promise<{ prk: Uint8Array<ArrayBuffer>; info: Uint8Array<ArrayBuffer>; key: Uint8Array<ArrayBuffer> }> {
	checkGeneration(credential);
	checkPhysicalName(physicalName);
	if (domain !== DOMAIN_GROUP && domain !== DOMAIN_DATAGRAM) {
		throw new ProfileError("identity", `unknown domain ${domain}`);
	}
	const extracted = prk ?? (await extract(credential));
	const info = keyInfo(credential, physicalName, domain);
	const key = await hkdfExpand(extracted, info, KEY_LEN);
	return { prk: extracted, info, key };
}

export async function protect(
	key: Uint8Array<ArrayBuffer>,
	group: bigint,
	frame: number,
	plaintext: Uint8Array<ArrayBuffer>,
	payloadLimit: number,
): Promise<Uint8Array<ArrayBuffer>> {
	checkIdentity(group, frame);
	if (plaintext.length + TAG_LEN > payloadLimit) throw new ProfileError("oversize");
	const cryptoKey = await crypto.subtle.importKey("raw", key, "AES-GCM", false, ["encrypt"]);
	const sealed = await crypto.subtle.encrypt(
		{ name: "AES-GCM", iv: nonce(group, frame), tagLength: 128, additionalData: new Uint8Array() },
		cryptoKey,
		plaintext,
	);
	return new Uint8Array(sealed);
}

export async function unprotect(
	key: Uint8Array<ArrayBuffer>,
	group: bigint,
	frame: number,
	payload: Uint8Array<ArrayBuffer>,
	payloadLimit: number,
): Promise<Uint8Array<ArrayBuffer>> {
	checkIdentity(group, frame);
	if (payload.length < TAG_LEN || payload.length > payloadLimit) throw new ProfileError("oversize");
	try {
		const cryptoKey = await crypto.subtle.importKey("raw", key, "AES-GCM", false, ["decrypt"]);
		const opened = await crypto.subtle.decrypt(
			{ name: "AES-GCM", iv: nonce(group, frame), tagLength: 128, additionalData: new Uint8Array() },
			cryptoKey,
			payload,
		);
		return new Uint8Array(opened);
	} catch {
		throw new ProfileError("authentication");
	}
}

const EPOCH = "0199b7f4-3c2a-7d1e-9f0b-2b6c1a9d8e7f";
const OTHER_EPOCH = "0199b7f4-9e10-7f42-8a3c-5d7e2b1c0f9a";

function gen(epoch = EPOCH, secret = utf8("moq-e2ee-00 test secret!!!!!!!!!")): Generation {
	return {
		context: utf8("example.com/meeting-123"),
		epoch: utf8(epoch),
		kid: 7n,
		secret,
	};
}

function generationJson(generation: Generation) {
	return {
		context: hex(generation.context),
		epoch: new TextDecoder().decode(generation.epoch),
		kid: Number(generation.kid),
		secret: hex(generation.secret),
	};
}

async function derivationVector(id: string, generation: Generation, semanticName: string, domain: Domain) {
	const named = await deriveName(generation, utf8(semanticName));
	const keyed = await deriveKey(generation, named.physicalName, domain, named.prk);
	return {
		id,
		...generationJson(generation),
		semantic_name: semanticName,
		semantic_name_bytes: hex(utf8(semanticName)),
		domain,
		prk: hex(named.prk),
		name_info: hex(named.info),
		name_material: hex(named.material),
		physical_name: named.physicalName,
		key_info: hex(keyed.info),
		key: hex(keyed.key),
	};
}

async function payloadVector(
	id: string,
	generation: Generation,
	semanticName: string,
	domain: Domain,
	group: bigint,
	frame: number,
	plaintext: Uint8Array<ArrayBuffer>,
	payloadLimit: number,
) {
	const named = await deriveName(generation, utf8(semanticName));
	const keyed = await deriveKey(generation, named.physicalName, domain, named.prk);
	const iv = nonce(group, frame);
	const payload = await protect(keyed.key, group, frame, plaintext, payloadLimit);
	return {
		id,
		...generationJson(generation),
		semantic_name: semanticName,
		physical_name: named.physicalName,
		domain,
		group: Number(group),
		frame,
		nonce: hex(iv),
		plaintext: hex(plaintext),
		payload: hex(payload),
		payload_limit: payloadLimit,
		key: hex(keyed.key),
	};
}

async function generate() {
	const base = gen();
	const video = await deriveName(base, utf8("video"));

	const derivation = [
		await derivationVector("group-video", base, "video", DOMAIN_GROUP),
		await derivationVector("datagram-audio", base, "audio", DOMAIN_DATAGRAM),
		await derivationVector("catalog-json", base, "catalog.json", DOMAIN_GROUP),
		await derivationVector("other-epoch", gen(OTHER_EPOCH), "video", DOMAIN_GROUP),
	];

	const naming = derivation.map((row) => ({
		id: row.id,
		epoch: row.epoch,
		semantic_name: row.semantic_name,
		physical_name: row.physical_name,
		name_material: row.name_material,
	}));

	const groups = [
		await payloadVector("group-0", base, "video", DOMAIN_GROUP, 42n, 0, utf8("frame-zero"), MAX_GROUPED_PAYLOAD),
		await payloadVector("group-1", base, "video", DOMAIN_GROUP, 42n, 1, utf8("frame-one"), MAX_GROUPED_PAYLOAD),
		await payloadVector("group-next", base, "video", DOMAIN_GROUP, 43n, 0, utf8("next-group"), MAX_GROUPED_PAYLOAD),
		await payloadVector("empty", base, "video", DOMAIN_GROUP, 1n, 0, new Uint8Array(), MAX_GROUPED_PAYLOAD),
		// A restarted publisher mints a new epoch and reuses the transport identity safely.
		await payloadVector(
			"restart-new-epoch",
			gen(OTHER_EPOCH),
			"video",
			DOMAIN_GROUP,
			42n,
			0,
			utf8("restarted"),
			MAX_GROUPED_PAYLOAD,
		),
	];

	const datagrams = [
		await payloadVector("datagram-0", base, "audio", DOMAIN_DATAGRAM, 99n, 0, utf8("opus"), MAX_DATAGRAM_PAYLOAD),
	];

	const videoKey = await deriveKey(base, video.physicalName, DOMAIN_GROUP, video.prk);
	const good = groups[0];
	const flipped = unhex(String(good.payload));
	flipped[0] ^= 0x01;

	const negative: NegativeVector[] = [];
	for (const [id, generation, name, domain, group, frame] of [
		["tag-failure", base, "video", DOMAIN_GROUP, 42, 0],
		["relocation-context", { ...base, context: utf8("other-broadcast") }, "video", DOMAIN_GROUP, 42, 0],
		["relocation-epoch", gen(OTHER_EPOCH), "video", DOMAIN_GROUP, 42, 0],
		["relocation-kid", { ...base, kid: 8n }, "video", DOMAIN_GROUP, 42, 0],
		["relocation-track", base, "audio", DOMAIN_GROUP, 42, 0],
		["relocation-domain", base, "video", DOMAIN_DATAGRAM, 42, 0],
		["relocation-group", base, "video", DOMAIN_GROUP, 43, 0],
		["relocation-frame", base, "video", DOMAIN_GROUP, 42, 1],
	] as const) {
		// Hold the physical name fixed while relocating each generation field.
		const named = await deriveName(base, utf8(name));
		const keyed = await deriveKey(generation, named.physicalName, domain);
		negative.push({
			id,
			operation: "open",
			error: "authentication",
			generation: generationJson(generation),
			physical_name: named.physicalName,
			domain,
			group,
			frame,
			key: hex(keyed.key),
			payload: id === "tag-failure" ? hex(flipped) : good.payload,
			payload_limit: MAX_GROUPED_PAYLOAD,
		});
	}
	for (const [id, group, frame] of [
		["frame-exhausted", "0", MAX_U32 + 1],
		["group-exhausted", String(BigInt(MAX_U53) + 1n), 0],
		["negative-group", "-1", 0],
		["negative-frame", "0", -1],
		["fractional-frame", "0", 1.5],
		["nan-frame", "0", "NaN"],
		["infinite-frame", "0", "Infinity"],
		["negative-infinite-frame", "0", "-Infinity"],
	] as const) {
		negative.push({ id, operation: "identity", error: "identity", group, frame });
	}
	for (const [id, generation, error] of [
		["invalid-secret", { ...base, secret: new Uint8Array(16) }, "invalid_secret"],
		["kid-exhausted", { ...base, kid: BigInt(MAX_U53) + 1n }, "identity"],
		["epoch-empty", { ...base, epoch: new Uint8Array() }, "identity"],
		["epoch-slash", { ...base, epoch: utf8("2026/09") }, "identity"],
	] as const) {
		negative.push({ id, operation: "generation", error, generation: generationJson(generation) });
	}
	for (const [id, physicalName] of [
		["physical-name-short", "abc"],
		["physical-name-long", "a".repeat(23)],
		["physical-name-charset", "!!!!!!!!!!!!!!!!!!!!!!"],
		["physical-name-padded", "abcdefghijklmnopqrstu="],
	] as const) {
		negative.push({
			id,
			operation: "key",
			error: "identity",
			generation: generationJson(base),
			physical_name: physicalName,
			domain: DOMAIN_GROUP,
		});
	}
	for (const field of ["context", "epoch", "semantic_name"] as const) {
		negative.push({
			id: `${field.replaceAll("_", "-")}-too-long`,
			operation: "bytes",
			error: "identity",
			field,
			length: 0x10000,
		});
	}
	negative.push({
		id: "grouped-oversize",
		operation: "protect",
		error: "oversize",
		key: hex(videoKey.key),
		group: 0,
		frame: 0,
		plaintext_len: MAX_GROUPED_PLAINTEXT + 1,
		payload_limit: MAX_GROUPED_PAYLOAD,
	});
	negative.push({
		id: "datagram-oversize",
		operation: "protect",
		error: "oversize",
		key: datagrams[0].key,
		group: 99,
		frame: 0,
		plaintext_len: MAX_DATAGRAM_PLAINTEXT + 1,
		payload_limit: MAX_DATAGRAM_PAYLOAD,
	});
	negative.push({
		id: "short-payload",
		operation: "open",
		error: "oversize",
		generation: generationJson(base),
		physical_name: video.physicalName,
		domain: DOMAIN_GROUP,
		group: 0,
		frame: 0,
		key: hex(videoKey.key),
		payload: "00",
		payload_limit: MAX_GROUPED_PAYLOAD,
	});

	return {
		profile: PROFILE,
		constants: {
			salt: hex(SALT),
			name_label: hex(NAME_LABEL),
			key_label: hex(KEY_LABEL),
			domain_group: DOMAIN_GROUP,
			domain_datagram: DOMAIN_DATAGRAM,
			tag_len: TAG_LEN,
			key_len: KEY_LEN,
			name_len: NAME_LEN,
			max_u53: MAX_U53,
			max_u32: MAX_U32,
			max_invocations: MAX_INVOCATIONS,
			max_plaintext_bytes: MAX_PLAINTEXT_BYTES,
			max_grouped_payload: MAX_GROUPED_PAYLOAD,
			max_grouped_plaintext: MAX_GROUPED_PLAINTEXT,
			max_datagram_body: MAX_DATAGRAM_BODY,
			max_datagram_header: MAX_DATAGRAM_HEADER,
			max_datagram_payload: MAX_DATAGRAM_PAYLOAD,
			max_datagram_plaintext: MAX_DATAGRAM_PLAINTEXT,
		},
		generation: generationJson(base),
		derivation,
		naming,
		groups,
		datagrams,
		negative,
	};
}

type NegativeVector = { id: string; error: string } & (
	| {
			operation: "open";
			generation: ReturnType<typeof generationJson>;
			physical_name: string;
			domain: Domain;
			group: number;
			frame: number;
			key: string;
			payload: string;
			payload_limit: number;
	  }
	| {
			operation: "protect";
			key: string;
			group: number;
			frame: number;
			plaintext_len: number;
			payload_limit: number;
	  }
	| {
			operation: "key";
			generation: ReturnType<typeof generationJson>;
			physical_name: string;
			domain: Domain;
	  }
	| { operation: "identity"; group: string; frame: number | "NaN" | "Infinity" | "-Infinity" }
	| { operation: "generation"; generation: ReturnType<typeof generationJson> }
	| { operation: "bytes"; field: "context" | "epoch" | "semantic_name"; length: number }
);

function parseGeneration(row: ReturnType<typeof generationJson>): Generation {
	return {
		context: unhex(row.context),
		epoch: utf8(row.epoch),
		kid: BigInt(row.kid),
		secret: unhex(row.secret),
	};
}

function pathFor(file: string): string {
	return join(dirname(fileURLToPath(import.meta.url)), file);
}

async function verify(doc: Awaited<ReturnType<typeof generate>>): Promise<void> {
	if (doc.profile !== PROFILE) throw new Error(`profile ${doc.profile}`);

	for (const row of doc.derivation) {
		const generation = parseGeneration(row);
		const named = await deriveName(generation, utf8(String(row.semantic_name)));
		const keyed = await deriveKey(generation, named.physicalName, Number(row.domain) as Domain, named.prk);
		assertEqual("prk", hex(named.prk), String(row.prk));
		assertEqual("name_info", hex(named.info), String(row.name_info));
		assertEqual("name_material", hex(named.material), String(row.name_material));
		assertEqual("physical_name", named.physicalName, String(row.physical_name));
		assertEqual("key_info", hex(keyed.info), String(row.key_info));
		assertEqual("key", hex(keyed.key), String(row.key));
	}

	for (const row of doc.naming) {
		const generation = { ...parseGeneration(doc.generation), epoch: utf8(row.epoch) };
		const named = await deriveName(generation, utf8(row.semantic_name));
		assertEqual(`${row.id} naming`, named.physicalName, row.physical_name);
		assertEqual(`${row.id} name material`, hex(named.material), row.name_material);
	}

	for (const row of [...doc.groups, ...doc.datagrams]) {
		const generation = parseGeneration(row);
		const named = await deriveName(generation, utf8(String(row.semantic_name)));
		const keyed = await deriveKey(generation, named.physicalName, Number(row.domain) as Domain, named.prk);
		assertEqual(`${row.id} name`, named.physicalName, row.physical_name);
		assertEqual(`${row.id} key`, hex(keyed.key), row.key);
		assertEqual(`${row.id} nonce`, hex(nonce(BigInt(row.group), row.frame)), row.nonce);
		const group = BigInt(row.group);
		const frame = Number(row.frame);
		const limit = Number(row.payload_limit);
		const sealed = await protect(keyed.key, group, frame, unhex(String(row.plaintext)), limit);
		assertEqual(`${row.id} payload`, hex(sealed), String(row.payload));
		const opened = await unprotect(keyed.key, group, frame, sealed, limit);
		assertEqual(`${row.id} round-trip`, hex(opened), String(row.plaintext));
	}

	// The same transport identity under two epochs is two ciphertexts, and neither opens the other.
	const first = doc.groups.find((row) => row.id === "group-0");
	const restarted = doc.groups.find((row) => row.id === "restart-new-epoch");
	if (!first || !restarted) throw new Error("missing epoch vectors");
	if (first.key === restarted.key) throw new Error("epoch did not change the key");
	await expectError("authentication", () =>
		unprotect(unhex(String(first.key)), 42n, 0, unhex(String(restarted.payload)), MAX_GROUPED_PAYLOAD),
	);

	for (const row of doc.datagrams) {
		assertEqual("datagram payload budget", String(row.payload_limit), String(MAX_DATAGRAM_PAYLOAD));
		const key = unhex(String(row.key));
		await protect(key, BigInt(row.group), 0, new Uint8Array(MAX_DATAGRAM_PLAINTEXT), MAX_DATAGRAM_PAYLOAD);
		await expectError("oversize", () =>
			protect(key, BigInt(row.group), 0, new Uint8Array(MAX_DATAGRAM_PLAINTEXT + 1), MAX_DATAGRAM_PAYLOAD),
		);
	}

	for (const row of doc.negative) {
		await expectError(row.error, async () => {
			switch (row.operation) {
				case "open": {
					const keyed = await deriveKey(parseGeneration(row.generation), row.physical_name, row.domain);
					assertEqual(`${row.id} key`, hex(keyed.key), row.key);
					await unprotect(keyed.key, BigInt(row.group), row.frame, unhex(row.payload), row.payload_limit);
					break;
				}
				case "protect":
					await protect(
						unhex(row.key),
						BigInt(row.group),
						row.frame,
						new Uint8Array(row.plaintext_len),
						row.payload_limit,
					);
					break;
				case "key":
					await deriveKey(parseGeneration(row.generation), row.physical_name, row.domain);
					break;
				case "identity":
					nonce(BigInt(row.group), Number(row.frame));
					break;
				case "generation":
					checkGeneration(parseGeneration(row.generation));
					break;
				case "bytes": {
					const oversized = new Uint8Array(row.length).fill(0x61);
					const generation = parseGeneration(doc.generation);
					if (row.field === "semantic_name") {
						await deriveName(generation, oversized);
					} else {
						checkGeneration({ ...generation, [row.field]: oversized });
					}
					break;
				}
				default:
					throw new Error(`unknown negative operation: ${JSON.stringify(row)}`);
			}
		});
	}
}

function assertEqual(label: string, actual: string, expected: string): void {
	if (actual !== expected) throw new Error(`${label}: ${actual} != ${expected}`);
}

async function expectError(code: string, fn: () => unknown | Promise<unknown>): Promise<void> {
	try {
		await fn();
	} catch (error) {
		if (error instanceof ProfileError && error.code === code) return;
		throw error;
	}
	throw new Error(`expected ${code}`);
}

async function main(): Promise<void> {
	const write = process.argv.includes("--write");
	const generated = await generate();
	const jsonPath = pathFor("moq-e2ee-00.json");
	if (write) {
		mkdirSync(dirname(jsonPath), { recursive: true });
		writeFileSync(jsonPath, `${JSON.stringify(generated, null, "\t")}\n`);
	}
	const onDisk = JSON.parse(readFileSync(jsonPath, "utf8")) as Awaited<ReturnType<typeof generate>>;
	await verify(onDisk);
	if (JSON.stringify(onDisk) !== JSON.stringify(generated)) {
		throw new Error("drafts/moq-e2ee-00.json is stale; run bun drafts/moq-e2ee-00.ts --write");
	}
	console.log(
		`moq-e2ee-00: ${onDisk.derivation.length} derivation, ${onDisk.groups.length} group, ${onDisk.datagrams.length} datagram, ${onDisk.negative.length} negative vectors`,
	);
}

if (import.meta.main) {
	await main();
}
