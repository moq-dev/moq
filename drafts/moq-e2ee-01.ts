#!/usr/bin/env bun
/**
 * moq-e2ee-01 primitives and known-answer vectors.
 *
 * `bun drafts/moq-e2ee-01.ts` verifies drafts/moq-e2ee-01.json against this
 * file. `bun drafts/moq-e2ee-01.ts --write` regenerates the JSON.
 *
 * This is the language-neutral contract, not the TypeScript E2EE core: it does
 * not wrap groups, datagrams, or catalogs.
 */

import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

export const PROFILE = "moq-e2ee-01";
export const SALT = utf8("moq-e2ee-01");
export const NAME_LABEL = utf8("moq-e2ee-01 name");
export const KEY_LABEL = utf8("moq-e2ee-01 key");
export const DOMAIN_GROUP = 0x00;
export const DOMAIN_DATAGRAM = 0x01;
export const TAG_LEN = 16;
export const KEY_LEN = 16;
export const NAME_LEN = 16;
export const HASH_LEN = 32;
export const MAX_U53 = Number.MAX_SAFE_INTEGER;
export const MAX_U32 = 2 ** 32 - 1;
export const MAX_INVOCATIONS = 2 ** 24;
export const MAX_GROUPED_PAYLOAD = 32 * 1024 * 1024;
export const MAX_GROUPED_PLAINTEXT = MAX_GROUPED_PAYLOAD - TAG_LEN;
export const MAX_DATAGRAM_BODY = 1200;

export type Domain = typeof DOMAIN_GROUP | typeof DOMAIN_DATAGRAM;

export type Credential = {
	context: Uint8Array;
	generation: bigint;
	kid: bigint;
	secret: Uint8Array;
};

export class ProfileError extends Error {
	readonly code: string;
	constructor(code: string, message = code) {
		super(message);
		this.code = code;
	}
}

export function utf8(value: string): Uint8Array {
	return new TextEncoder().encode(value);
}

export function hex(bytes: Uint8Array): string {
	return [...bytes].map((b) => b.toString(16).padStart(2, "0")).join("");
}

export function unhex(value: string): Uint8Array {
	if (value.length % 2 !== 0) throw new Error(`odd hex length: ${value.length}`);
	const out = new Uint8Array(value.length / 2);
	for (let i = 0; i < out.length; i++) {
		out[i] = Number.parseInt(value.slice(i * 2, i * 2 + 2), 16);
	}
	return out;
}

export function concat(...parts: Uint8Array[]): Uint8Array {
	const out = new Uint8Array(parts.reduce((n, p) => n + p.length, 0));
	let offset = 0;
	for (const part of parts) {
		out.set(part, offset);
		offset += part.length;
	}
	return out;
}

export function encodeU16(value: number): Uint8Array {
	if (value < 0 || value > 0xffff) throw new ProfileError("identity", `u16 out of range: ${value}`);
	return new Uint8Array([(value >> 8) & 0xff, value & 0xff]);
}

export function encodeU32(value: number): Uint8Array {
	if (value < 0 || value > MAX_U32) throw new ProfileError("identity", `u32 out of range: ${value}`);
	const out = new Uint8Array(4);
	new DataView(out.buffer).setUint32(0, value);
	return out;
}

export function encodeU64(value: bigint): Uint8Array {
	if (value < 0n || value > 0xffffffffffffffffn) {
		throw new ProfileError("identity", `u64 out of range: ${value}`);
	}
	const out = new Uint8Array(8);
	new DataView(out.buffer).setBigUint64(0, value);
	return out;
}

export function encodeBytes(value: Uint8Array): Uint8Array {
	if (value.length > 0xffff) throw new ProfileError("identity", `bytes too long: ${value.length}`);
	return concat(encodeU16(value.length), value);
}

export function base64url(bytes: Uint8Array): string {
	return btoa(String.fromCharCode(...bytes))
		.replaceAll("+", "-")
		.replaceAll("/", "_")
		.replaceAll("=", "");
}

export function checkCredential(credential: Credential): void {
	if (credential.secret.length !== 32) throw new ProfileError("invalid_secret");
	checkU53(credential.generation, "generation");
	checkU53(credential.kid, "kid");
}

function checkU53(value: bigint, label: string): void {
	if (value < 0n || value > BigInt(MAX_U53)) {
		throw new ProfileError("identity", `${label} exceeds MAX_SAFE_INTEGER`);
	}
}

export function checkIdentity(group: bigint, frame: number): void {
	checkU53(group, "group");
	if (frame < 0 || frame > MAX_U32) throw new ProfileError("identity", "frame exceeds 32 bits");
}

export function nonce(group: bigint, frame: number): Uint8Array {
	checkIdentity(group, frame);
	return concat(encodeU64(group), encodeU32(frame));
}

async function hmacSha256(key: Uint8Array, data: Uint8Array): Promise<Uint8Array> {
	const cryptoKey = await crypto.subtle.importKey("raw", key, { name: "HMAC", hash: "SHA-256" }, false, ["sign"]);
	return new Uint8Array(await crypto.subtle.sign("HMAC", cryptoKey, data));
}

export async function extract(credential: Credential, salt = SALT): Promise<Uint8Array> {
	checkCredential(credential);
	return hmacSha256(salt, credential.secret);
}

async function hkdfExpand(prk: Uint8Array, info: Uint8Array, length: number): Promise<Uint8Array> {
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

export function nameInfo(credential: Credential, semanticName: Uint8Array): Uint8Array {
	return concat(
		NAME_LABEL,
		encodeBytes(credential.context),
		encodeU64(credential.generation),
		encodeU64(credential.kid),
		encodeBytes(semanticName),
	);
}

export function keyInfo(credential: Credential, physicalName: string, domain: Domain): Uint8Array {
	return concat(
		KEY_LABEL,
		encodeBytes(credential.context),
		encodeU64(credential.generation),
		encodeU64(credential.kid),
		encodeBytes(utf8(physicalName)),
		new Uint8Array([domain]),
	);
}

export async function deriveName(
	credential: Credential,
	semanticName: Uint8Array,
	prk?: Uint8Array,
): Promise<{ prk: Uint8Array; info: Uint8Array; material: Uint8Array; physicalName: string }> {
	checkCredential(credential);
	const extracted = prk ?? (await extract(credential));
	const info = nameInfo(credential, semanticName);
	const material = await hkdfExpand(extracted, info, NAME_LEN);
	return { prk: extracted, info, material, physicalName: base64url(material) };
}

export async function deriveKey(
	credential: Credential,
	physicalName: string,
	domain: Domain,
	prk?: Uint8Array,
): Promise<{ prk: Uint8Array; info: Uint8Array; key: Uint8Array }> {
	checkCredential(credential);
	if (domain !== DOMAIN_GROUP && domain !== DOMAIN_DATAGRAM) {
		throw new ProfileError("identity", `unknown domain ${domain}`);
	}
	const extracted = prk ?? (await extract(credential));
	const info = keyInfo(credential, physicalName, domain);
	const key = await hkdfExpand(extracted, info, KEY_LEN);
	return { prk: extracted, info, key };
}

export async function protect(
	key: Uint8Array,
	group: bigint,
	frame: number,
	plaintext: Uint8Array,
	payloadLimit: number,
): Promise<Uint8Array> {
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
	key: Uint8Array,
	group: bigint,
	frame: number,
	payload: Uint8Array,
	payloadLimit: number,
): Promise<Uint8Array> {
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

type HexMap = Record<string, string | number | boolean>;

function cred(secret = utf8("moq-e2ee-01 test secret!!!!!!!!!")): Credential {
	return {
		context: utf8("example.com/meeting-123"),
		generation: 1n,
		kid: 7n,
		secret,
	};
}

function credentialJson(credential: Credential) {
	return {
		context: hex(credential.context),
		generation: Number(credential.generation),
		kid: Number(credential.kid),
		secret: hex(credential.secret),
	};
}

async function derivationVector(
	id: string,
	credential: Credential,
	semanticName: string,
	domain: Domain,
): Promise<HexMap> {
	const named = await deriveName(credential, utf8(semanticName));
	const keyed = await deriveKey(credential, named.physicalName, domain, named.prk);
	return {
		id,
		...credentialJson(credential),
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
	credential: Credential,
	semanticName: string,
	domain: Domain,
	group: bigint,
	frame: number,
	plaintext: Uint8Array,
	payloadLimit: number,
): Promise<HexMap> {
	const named = await deriveName(credential, utf8(semanticName));
	const keyed = await deriveKey(credential, named.physicalName, domain, named.prk);
	const iv = nonce(group, frame);
	const payload = await protect(keyed.key, group, frame, plaintext, payloadLimit);
	return {
		id,
		...credentialJson(credential),
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
	const base = cred();
	const catalogJson = utf8('{"video":{"renditions":{"hd":{"codec":"avc1"}}}}');
	const catalogCompressed = unhex("789c4b2c4d2c492d2e51d04b4d2c49cc2a4e2d2a2e294a4d2c4e2d2a2e010000ffff");

	const derivation = [
		await derivationVector("group-video", base, "video", DOMAIN_GROUP),
		await derivationVector("datagram-audio", base, "audio", DOMAIN_DATAGRAM),
		await derivationVector("catalog-json", base, "catalog.json", DOMAIN_GROUP),
		await derivationVector("catalog-json-z", base, "catalog.json.z", DOMAIN_GROUP),
	];

	const naming = derivation.map((row) => ({
		id: row.id,
		semantic_name: row.semantic_name,
		physical_name: row.physical_name,
		name_material: row.name_material,
	}));

	const groups = [
		await payloadVector("group-0", base, "video", DOMAIN_GROUP, 42n, 0, utf8("frame-zero"), MAX_GROUPED_PAYLOAD),
		await payloadVector("group-1", base, "video", DOMAIN_GROUP, 42n, 1, utf8("frame-one"), MAX_GROUPED_PAYLOAD),
		await payloadVector("group-next", base, "video", DOMAIN_GROUP, 43n, 0, utf8("next-group"), MAX_GROUPED_PAYLOAD),
		await payloadVector("empty", base, "video", DOMAIN_GROUP, 1n, 0, new Uint8Array(), MAX_GROUPED_PAYLOAD),
	];

	const datagrams = [
		await payloadVector("datagram-0", base, "audio", DOMAIN_DATAGRAM, 99n, 0, utf8("opus"), MAX_DATAGRAM_BODY),
	];

	const catalog = [
		await payloadVector(
			"catalog-json",
			base,
			"catalog.json",
			DOMAIN_GROUP,
			0n,
			0,
			catalogJson,
			MAX_GROUPED_PAYLOAD,
		),
		await payloadVector(
			"catalog-compressed",
			base,
			"catalog.json.z",
			DOMAIN_GROUP,
			0n,
			0,
			catalogCompressed,
			MAX_GROUPED_PAYLOAD,
		),
	];

	const video = await deriveName(base, utf8("video"));
	const videoKey = await deriveKey(base, video.physicalName, DOMAIN_GROUP, video.prk);
	const good = groups[0];
	const flipped = unhex(String(good.payload));
	flipped[0] ^= 0x01;

	const relocated = await unprotectCatch(videoKey.key, 42n, 1, unhex(String(good.payload)), MAX_GROUPED_PAYLOAD);
	const wrongTrack = await deriveName(base, utf8("audio"));
	const wrongKey = await deriveKey(base, wrongTrack.physicalName, DOMAIN_GROUP, wrongTrack.prk);
	const relocatedTrack = await unprotectCatch(wrongKey.key, 42n, 0, unhex(String(good.payload)), MAX_GROUPED_PAYLOAD);
	const datagramKey = await deriveKey(base, video.physicalName, DOMAIN_DATAGRAM, video.prk);
	const relocatedDomain = await unprotectCatch(
		datagramKey.key,
		42n,
		0,
		unhex(String(good.payload)),
		MAX_GROUPED_PAYLOAD,
	);

	const otherGen = { ...base, generation: 2n };
	const otherGenName = await deriveName(otherGen, utf8("video"));
	const otherGenKey = await deriveKey(otherGen, otherGenName.physicalName, DOMAIN_GROUP, otherGenName.prk);
	const restartOk = await protect(otherGenKey.key, 42n, 0, utf8("restarted"), MAX_GROUPED_PAYLOAD);

	const downgradePrk = await extract(base, utf8("moq-e2ee-00"));
	const downgradeInfo = nameInfo(base, utf8("video"));
	const downgradeMaterial = await hkdfExpand(downgradePrk, downgradeInfo, NAME_LEN);

	const negative = [
		{
			id: "tag-failure",
			error: "authentication",
			payload: hex(flipped),
			group: 42,
			frame: 0,
			key: hex(videoKey.key),
		},
		{
			id: "relocation-frame",
			error: relocated,
			payload: good.payload,
			group: 42,
			frame: 1,
			key: hex(videoKey.key),
		},
		{
			id: "relocation-track",
			error: relocatedTrack,
			payload: good.payload,
			group: 42,
			frame: 0,
			key: hex(wrongKey.key),
		},
		{
			id: "relocation-domain",
			error: relocatedDomain,
			payload: good.payload,
			group: 42,
			frame: 0,
			key: hex(datagramKey.key),
		},
		{
			id: "profile-downgrade",
			error: "authentication",
			note: "HKDF salt moq-e2ee-00 yields a different name and key than profile 01",
			downgrade_name_material: hex(downgradeMaterial),
			profile_name_material: hex(video.material),
		},
		{
			id: "frame-exhausted",
			error: "identity",
			group: 0,
			frame: MAX_U32 + 1,
		},
		{
			id: "group-exhausted",
			error: "identity",
			group: MAX_U53 + 1,
			frame: 0,
		},
		{
			id: "invocation-limit",
			error: "exhausted",
			max_invocations: MAX_INVOCATIONS,
		},
		{
			id: "grouped-oversize",
			error: "oversize",
			plaintext_len: MAX_GROUPED_PLAINTEXT + 1,
			payload_limit: MAX_GROUPED_PAYLOAD,
		},
		{
			id: "datagram-oversize",
			error: "oversize",
			plaintext_len: MAX_DATAGRAM_BODY - TAG_LEN + 1,
			payload_limit: MAX_DATAGRAM_BODY,
		},
		{
			id: "reuse",
			error: "reuse",
			identity: { semantic_name: "video", domain: DOMAIN_GROUP, group: 42, frame: 0 },
			first_plaintext: hex(utf8("frame-zero")),
			second_plaintext: hex(utf8("other-bytes")),
		},
		{
			id: "restart-same-generation",
			error: "reuse",
			note: "A publisher restart that would reuse transport sequence numbers under the same generation is reuse",
			identity: { semantic_name: "video", domain: DOMAIN_GROUP, group: 42, frame: 0 },
		},
		{
			id: "restart-new-generation",
			error: null,
			generation: 2,
			group: 42,
			frame: 0,
			plaintext: hex(utf8("restarted")),
			physical_name: otherGenName.physicalName,
			payload: hex(restartOk),
			key: hex(otherGenKey.key),
		},
		{
			id: "short-payload",
			error: "oversize",
			payload: "00",
		},
		{
			id: "invalid-secret",
			error: "invalid_secret",
			secret_len: 16,
		},
	];

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
			max_grouped_payload: MAX_GROUPED_PAYLOAD,
			max_grouped_plaintext: MAX_GROUPED_PLAINTEXT,
			max_datagram_body: MAX_DATAGRAM_BODY,
		},
		credential: credentialJson(base),
		derivation,
		naming,
		groups,
		datagrams,
		catalog,
		negative,
	};
}

async function unprotectCatch(
	key: Uint8Array,
	group: bigint,
	frame: number,
	payload: Uint8Array,
	limit: number,
): Promise<string> {
	try {
		await unprotect(key, group, frame, payload, limit);
		return "unexpected-success";
	} catch (error) {
		return error instanceof ProfileError ? error.code : "error";
	}
}

function pathFor(file: string): string {
	return join(dirname(fileURLToPath(import.meta.url)), file);
}

async function verify(doc: Awaited<ReturnType<typeof generate>>): Promise<void> {
	if (doc.profile !== PROFILE) throw new Error(`profile ${doc.profile}`);

	for (const row of doc.derivation) {
		const credential: Credential = {
			context: unhex(String(row.context)),
			generation: BigInt(row.generation),
			kid: BigInt(row.kid),
			secret: unhex(String(row.secret)),
		};
		const named = await deriveName(credential, utf8(String(row.semantic_name)));
		const keyed = await deriveKey(credential, named.physicalName, Number(row.domain) as Domain, named.prk);
		assertEqual("prk", hex(named.prk), String(row.prk));
		assertEqual("name_info", hex(named.info), String(row.name_info));
		assertEqual("name_material", hex(named.material), String(row.name_material));
		assertEqual("physical_name", named.physicalName, String(row.physical_name));
		assertEqual("key_info", hex(keyed.info), String(row.key_info));
		assertEqual("key", hex(keyed.key), String(row.key));
	}

	for (const row of [...doc.groups, ...doc.datagrams, ...doc.catalog]) {
		const credential: Credential = {
			context: unhex(String(row.context)),
			generation: BigInt(row.generation),
			kid: BigInt(row.kid),
			secret: unhex(String(row.secret)),
		};
		const named = await deriveName(credential, utf8(String(row.semantic_name)));
		const keyed = await deriveKey(credential, named.physicalName, Number(row.domain) as Domain, named.prk);
		const group = BigInt(row.group);
		const frame = Number(row.frame);
		const limit = Number(row.payload_limit);
		const sealed = await protect(keyed.key, group, frame, unhex(String(row.plaintext)), limit);
		assertEqual(`${row.id} payload`, hex(sealed), String(row.payload));
		const opened = await unprotect(keyed.key, group, frame, sealed, limit);
		assertEqual(`${row.id} round-trip`, hex(opened), String(row.plaintext));
	}

	const video = doc.groups[0];
	const key = unhex(String(video.key));
	await expectError("authentication", () =>
		unprotect(key, 42n, 0, unhex(String(doc.negative[0].payload)), MAX_GROUPED_PAYLOAD),
	);
	await expectError("authentication", () =>
		unprotect(key, 42n, 1, unhex(String(video.payload)), MAX_GROUPED_PAYLOAD),
	);
	await expectError("identity", () => nonce(0n, MAX_U32 + 1));
	await expectError("identity", () => nonce(BigInt(MAX_U53) + 1n, 0));
	await expectError("oversize", () =>
		protect(key, 0n, 0, new Uint8Array(MAX_GROUPED_PLAINTEXT + 1), MAX_GROUPED_PAYLOAD),
	);
	await expectError("oversize", () =>
		protect(key, 0n, 0, new Uint8Array(MAX_DATAGRAM_BODY - TAG_LEN + 1), MAX_DATAGRAM_BODY),
	);
	await expectError("oversize", () => unprotect(key, 0n, 0, new Uint8Array([0]), MAX_GROUPED_PAYLOAD));
	await expectError("invalid_secret", () => checkCredential({ ...cred(), secret: new Uint8Array(16) }));

	const restart = doc.negative.find((row) => row.id === "restart-new-generation");
	if (!restart || typeof restart.payload !== "string" || typeof restart.key !== "string") {
		throw new Error("missing restart-new-generation vector");
	}
	const opened = await unprotect(unhex(restart.key), 42n, 0, unhex(restart.payload), MAX_GROUPED_PAYLOAD);
	assertEqual("restart plaintext", hex(opened), String(restart.plaintext));
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
	const jsonPath = pathFor("moq-e2ee-01.json");
	if (write) {
		mkdirSync(dirname(jsonPath), { recursive: true });
		writeFileSync(jsonPath, `${JSON.stringify(generated, null, "\t")}\n`);
	}
	const onDisk = JSON.parse(readFileSync(jsonPath, "utf8")) as Awaited<ReturnType<typeof generate>>;
	await verify(onDisk);
	if (JSON.stringify(onDisk) !== JSON.stringify(generated)) {
		throw new Error("drafts/moq-e2ee-01.json is stale; run bun drafts/moq-e2ee-01.ts --write");
	}
	console.log(
		`moq-e2ee-01: ${onDisk.derivation.length} derivation, ${onDisk.groups.length} group, ${onDisk.datagrams.length} datagram, ${onDisk.catalog.length} catalog, ${onDisk.negative.length} negative vectors`,
	);
}

if (import.meta.main) {
	await main();
}
