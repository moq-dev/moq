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
export const PATH_LABEL = utf8("moq-e2ee-00 path");
export const NAME_MAC_LABEL = utf8("moq-e2ee-00 name-mac");
export const NAME_KEY_LABEL = utf8("moq-e2ee-00 name-key");
export const KEY_LABEL = utf8("moq-e2ee-00 key");
export const DOMAIN_GROUP = 0x00;
export const DOMAIN_DATAGRAM = 0x01;
export const TAG_LEN = 16;
export const KEY_LEN = 16;
export const PATH_LEN = 16;
export const NAME_MAC_LEN = 32;
export const NAME_NONCE_LEN = 12;
export const NAME_BLOCK = 16;
export const HASH_LEN = 32;
export const MAX_U53 = Number.MAX_SAFE_INTEGER;
export const MAX_U32 = 2 ** 32 - 1;
export const MAX_INVOCATIONS = 2 ** 24;
export const MAX_PLAINTEXT_BYTES = 2 ** 36;
export const MAX_GROUPED_PAYLOAD = 32 * 1024 * 1024;
export const MAX_GROUPED_PLAINTEXT = MAX_GROUPED_PAYLOAD - TAG_LEN;
export const MAX_DATAGRAM_BODY = 1200;
// Lite-07's Type and three leading-ones varints at their widest: the publisher cannot see the Subscribe ID each hop encodes.
export const MAX_DATAGRAM_HEADER = 28;
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
	// Check bytes, not decoded length: TextDecoder strips a leading BOM.
	const epoch = new TextDecoder().decode(generation.epoch);
	if (
		generation.epoch.length !== 36 ||
		!/^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(epoch)
	) {
		throw new ProfileError("identity", "epoch is not canonical UUIDv7 text");
	}
}

function checkU53(value: bigint, label: string): void {
	if (value < 0n || value > BigInt(MAX_U53)) {
		throw new ProfileError("identity", `${label} exceeds MAX_SAFE_INTEGER`);
	}
}

export function unbase64url(text: string): Uint8Array<ArrayBuffer> {
	if (!/^[A-Za-z0-9_-]*$/.test(text)) throw new ProfileError("identity", `not base64url: ${text}`);
	let bytes: Uint8Array<ArrayBuffer>;
	try {
		bytes = Uint8Array.from(atob(text.replaceAll("-", "+").replaceAll("_", "/")), (c) => c.charCodeAt(0));
	} catch {
		throw new ProfileError("identity", `not base64url: ${text}`);
	}
	// Refuses nonzero trailing bits, so each name has one spelling.
	if (base64url(bytes) !== text) throw new ProfileError("identity", `noncanonical base64url: ${text}`);
	return bytes;
}

/** Decode a physical name: a 12-byte nonce, a whole number of padded blocks, and the tag. */
export function checkPhysicalName(name: string): Uint8Array<ArrayBuffer> {
	const material = unbase64url(name);
	const padded = material.length - NAME_NONCE_LEN - TAG_LEN;
	if (padded < NAME_BLOCK || padded % NAME_BLOCK !== 0) {
		throw new ProfileError("identity", `malformed physical name: ${name}`);
	}
	return material;
}

/** `semantic || 0x80 || 0x00...`, the fewest zeros that reach a multiple of 16 bytes. */
export function pad(semanticName: Uint8Array<ArrayBuffer>): Uint8Array<ArrayBuffer> {
	if (semanticName.length > 0xffff) throw new ProfileError("identity", "semantic name exceeds 65535 bytes");
	const out = new Uint8Array((Math.floor(semanticName.length / NAME_BLOCK) + 1) * NAME_BLOCK);
	out.set(semanticName);
	out[semanticName.length] = 0x80;
	return out;
}

/** Strip canonical padding, refusing any other. */
export function unpad(padded: Uint8Array<ArrayBuffer>): Uint8Array<ArrayBuffer> {
	let end = padded.length - 1;
	while (end >= 0 && padded[end] === 0) end--;
	if (end < 0 || padded[end] !== 0x80 || padded.length - end > NAME_BLOCK) {
		throw new ProfileError("authentication", "noncanonical name padding");
	}
	return padded.slice(0, end);
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

/** `bytes(context) || bytes(epoch) || u64(kid)`, bound by every epoch-scoped derivation. */
export function scope(generation: Generation): Uint8Array<ArrayBuffer> {
	return concat(encodeBytes(generation.context), encodeBytes(generation.epoch), encodeU64(generation.kid));
}

export function nameMacInfo(generation: Generation): Uint8Array<ArrayBuffer> {
	return concat(NAME_MAC_LABEL, scope(generation));
}

export function nameKeyInfo(generation: Generation): Uint8Array<ArrayBuffer> {
	return concat(NAME_KEY_LABEL, scope(generation));
}

/** Canonical path bytes for a credential and semantic name, excluding the epoch. */
export function pathInfo(credential: Credential, semanticName: Uint8Array<ArrayBuffer>): Uint8Array<ArrayBuffer> {
	return concat(PATH_LABEL, encodeBytes(credential.context), encodeU64(credential.kid), encodeBytes(semanticName));
}

export function keyInfo(
	generation: Generation,
	semanticName: Uint8Array<ArrayBuffer>,
	domain: Domain,
): Uint8Array<ArrayBuffer> {
	return concat(KEY_LABEL, scope(generation), encodeBytes(semanticName), new Uint8Array([domain]));
}

export type NameKeys = {
	prk: Uint8Array<ArrayBuffer>;
	macInfo: Uint8Array<ArrayBuffer>;
	keyInfo: Uint8Array<ArrayBuffer>;
	mac: Uint8Array<ArrayBuffer>;
	key: Uint8Array<ArrayBuffer>;
};

export async function deriveNameKeys(generation: Generation, prk?: Uint8Array<ArrayBuffer>): Promise<NameKeys> {
	checkGeneration(generation);
	const extracted = prk ?? (await extract(generation));
	const macInfo = nameMacInfo(generation);
	const keyInfo = nameKeyInfo(generation);
	return {
		prk: extracted,
		macInfo,
		keyInfo,
		mac: await hkdfExpand(extracted, macInfo, NAME_MAC_LEN),
		key: await hkdfExpand(extracted, keyInfo, KEY_LEN),
	};
}

async function nameNonce(keys: NameKeys, padded: Uint8Array<ArrayBuffer>): Promise<Uint8Array<ArrayBuffer>> {
	return (await hmacSha256(keys.mac, padded)).slice(0, NAME_NONCE_LEN);
}

async function aesGcm(
	mode: "encrypt" | "decrypt",
	key: Uint8Array<ArrayBuffer>,
	iv: Uint8Array<ArrayBuffer>,
	data: Uint8Array<ArrayBuffer>,
): Promise<Uint8Array<ArrayBuffer>> {
	const cryptoKey = await crypto.subtle.importKey("raw", key, "AES-GCM", false, [mode]);
	const params = { name: "AES-GCM", iv, tagLength: TAG_LEN * 8, additionalData: new Uint8Array() };
	return new Uint8Array(await crypto.subtle[mode](params, cryptoKey, data));
}

/** Seal `padded` under `nonce`; only {@link deriveName} and forged vectors choose their own. */
export async function sealName(
	keys: NameKeys,
	padded: Uint8Array<ArrayBuffer>,
	nonce?: Uint8Array<ArrayBuffer>,
): Promise<string> {
	const iv = nonce ?? (await nameNonce(keys, padded));
	return base64url(concat(iv, await aesGcm("encrypt", keys.key, iv, padded)));
}

export async function deriveName(
	generation: Generation,
	semanticName: Uint8Array<ArrayBuffer>,
	prk?: Uint8Array<ArrayBuffer>,
): Promise<
	NameKeys & {
		padded: Uint8Array<ArrayBuffer>;
		nonce: Uint8Array<ArrayBuffer>;
		physicalName: string;
	}
> {
	const keys = await deriveNameKeys(generation, prk);
	const padded = pad(semanticName);
	const nonce = await nameNonce(keys, padded);
	return { ...keys, padded, nonce, physicalName: await sealName(keys, padded, nonce) };
}

/** Recover the semantic name of a physical name issued under `generation`. */
export async function openName(generation: Generation, physicalName: string): Promise<Uint8Array<ArrayBuffer>> {
	const material = checkPhysicalName(physicalName);
	const keys = await deriveNameKeys(generation);
	const nonce = material.slice(0, NAME_NONCE_LEN);
	let padded: Uint8Array<ArrayBuffer>;
	try {
		padded = await aesGcm("decrypt", keys.key, nonce, material.slice(NAME_NONCE_LEN));
	} catch {
		throw new ProfileError("authentication", "physical name does not open");
	}
	// One physical name per semantic name, so two tracks never share its payload keys.
	if (hex(await nameNonce(keys, padded)) !== hex(nonce)) {
		throw new ProfileError("authentication", "physical name nonce mismatch");
	}
	const semantic = unpad(padded);
	try {
		new TextDecoder("utf-8", { fatal: true, ignoreBOM: true }).decode(semantic);
	} catch {
		throw new ProfileError("authentication", "semantic name is not UTF-8");
	}
	return semantic;
}

/** Derive the opaque broadcast prefix for a semantic name, independent of epoch. */
export async function derivePath(
	credential: Credential,
	semanticName: Uint8Array<ArrayBuffer>,
	prk?: Uint8Array<ArrayBuffer>,
): Promise<{
	prk: Uint8Array<ArrayBuffer>;
	info: Uint8Array<ArrayBuffer>;
	material: Uint8Array<ArrayBuffer>;
	opaque: string;
}> {
	checkCredential(credential);
	const extracted = prk ?? (await extract(credential));
	const info = pathInfo(credential, semanticName);
	const material = await hkdfExpand(extracted, info, PATH_LEN);
	return { prk: extracted, info, material, opaque: base64url(material) };
}

export async function deriveKey(
	credential: Generation,
	semanticName: Uint8Array<ArrayBuffer>,
	domain: Domain,
	prk?: Uint8Array<ArrayBuffer>,
): Promise<{ prk: Uint8Array<ArrayBuffer>; info: Uint8Array<ArrayBuffer>; key: Uint8Array<ArrayBuffer> }> {
	checkGeneration(credential);
	if (domain !== DOMAIN_GROUP && domain !== DOMAIN_DATAGRAM) {
		throw new ProfileError("identity", `unknown domain ${domain}`);
	}
	const extracted = prk ?? (await extract(credential));
	const info = keyInfo(credential, semanticName, domain);
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
		epoch: new TextDecoder("utf-8", { ignoreBOM: true }).decode(generation.epoch),
		kid: Number(generation.kid),
		secret: hex(generation.secret),
	};
}

function credentialJson(credential: Credential) {
	return {
		context: hex(credential.context),
		kid: Number(credential.kid),
		secret: hex(credential.secret),
	};
}

async function pathVector(id: string, credential: Credential, semanticName: string) {
	const path = await derivePath(credential, utf8(semanticName));
	return {
		id,
		...credentialJson(credential),
		semantic_name: semanticName,
		semantic_name_bytes: hex(utf8(semanticName)),
		prk: hex(path.prk),
		path_info: hex(path.info),
		path_material: hex(path.material),
		opaque: path.opaque,
	};
}

async function derivationVector(id: string, generation: Generation, semanticName: string, domain: Domain) {
	const semantic = utf8(semanticName);
	const named = await deriveName(generation, semantic);
	const keyed = await deriveKey(generation, semantic, domain, named.prk);
	return {
		id,
		...generationJson(generation),
		semantic_name: semanticName,
		semantic_name_bytes: hex(semantic),
		domain,
		prk: hex(named.prk),
		name_mac_info: hex(named.macInfo),
		name_mac: hex(named.mac),
		name_key_info: hex(named.keyInfo),
		name_key: hex(named.key),
		padded: hex(named.padded),
		name_nonce: hex(named.nonce),
		physical_name: named.physicalName,
		key_info: hex(keyed.info),
		key: hex(keyed.key),
	};
}

async function namingVector(id: string, generation: Generation, semantic: Uint8Array<ArrayBuffer>) {
	const named = await deriveName(generation, semantic);
	return {
		id,
		epoch: generationJson(generation).epoch,
		semantic_name: new TextDecoder().decode(semantic),
		semantic_name_bytes: hex(semantic),
		padded: hex(named.padded),
		physical_name: named.physicalName,
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
	const keyed = await deriveKey(generation, utf8(semanticName), domain, named.prk);
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

/** `length` bytes of `0123456789abcdef` repeated, to land on each side of a padding boundary. */
function sized(length: number): Uint8Array<ArrayBuffer> {
	return utf8("0123456789abcdef".repeat(Math.ceil(length / 16)).slice(0, length));
}

async function generate() {
	const base = gen();
	const video = await deriveName(base, utf8("video"));
	const paths = [await pathVector("broadcast-meeting-hang", base, "meeting.hang")];

	const derivation = [
		await derivationVector("group-video", base, "video", DOMAIN_GROUP),
		await derivationVector("datagram-audio", base, "audio", DOMAIN_DATAGRAM),
		await derivationVector("catalog-json", base, "catalog.json", DOMAIN_GROUP),
		await derivationVector("other-epoch", gen(OTHER_EPOCH), "video", DOMAIN_GROUP),
	];

	const naming = [
		...derivation.map((row) => ({
			id: row.id,
			epoch: row.epoch,
			semantic_name: row.semantic_name,
			semantic_name_bytes: row.semantic_name_bytes,
			padded: row.padded,
			physical_name: row.physical_name,
		})),
		await namingVector("length-0", base, new Uint8Array()),
		// 15 and 31 bytes fill a block with the marker; 16 and 32 bytes spill into another.
		await namingVector("length-15", base, sized(15)),
		await namingVector("length-16", base, sized(16)),
		await namingVector("length-31", base, sized(31)),
		await namingVector("length-32", base, sized(32)),
	];

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

	const videoKey = await deriveKey(base, utf8("video"), DOMAIN_GROUP, video.prk);
	const good = groups[0];
	const flipped = unhex(String(good.payload));
	flipped[0] ^= 0x01;

	const negative: NegativeVector[] = [];
	for (const [id, generation, semantic, domain, group, frame] of [
		["tag-failure", base, "video", DOMAIN_GROUP, 42, 0],
		["relocation-context", { ...base, context: utf8("other-broadcast") }, "video", DOMAIN_GROUP, 42, 0],
		["relocation-epoch", gen(OTHER_EPOCH), "video", DOMAIN_GROUP, 42, 0],
		["relocation-kid", { ...base, kid: 8n }, "video", DOMAIN_GROUP, 42, 0],
		["relocation-track", base, "audio", DOMAIN_GROUP, 42, 0],
		["relocation-domain", base, "video", DOMAIN_DATAGRAM, 42, 0],
		["relocation-group", base, "video", DOMAIN_GROUP, 43, 0],
		["relocation-frame", base, "video", DOMAIN_GROUP, 42, 1],
	] as const) {
		const keyed = await deriveKey(generation, utf8(semantic), domain);
		negative.push({
			id,
			operation: "open",
			error: "authentication",
			generation: generationJson(generation),
			semantic_name: semantic,
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
		["epoch-bom", { ...base, epoch: utf8(`﻿${EPOCH}`) }, "identity"],
	] as const) {
		negative.push({ id, operation: "generation", error, generation: generationJson(generation) });
	}

	// Physical names a relay could request: malformed ones fail to parse, the rest fail to open.
	const forged = checkPhysicalName(video.physicalName);
	forged[NAME_NONCE_LEN] ^= 0x01;
	const otherNonce = video.nonce.slice();
	otherNonce[0] ^= 0x01;
	const overlong = new Uint8Array(2 * NAME_BLOCK);
	overlong.set(pad(utf8("video")));
	const unmarked = new Uint8Array(NAME_BLOCK);
	unmarked.set(utf8("video"));
	// A 44-byte name leaves two zero bits in its last character.
	const alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
	const last = alphabet.indexOf(video.physicalName.slice(-1));
	const trailing = video.physicalName.slice(0, -1) + alphabet[last | 0x01];
	for (const [id, physicalName, error] of [
		["physical-name-short", "abc", "identity"],
		["physical-name-charset", "!".repeat(video.physicalName.length), "identity"],
		["physical-name-padded", `${video.physicalName}=`, "identity"],
		["physical-name-trailing-bits", trailing, "identity"],
		["physical-name-no-block", base64url(new Uint8Array(NAME_NONCE_LEN + TAG_LEN)), "identity"],
		["physical-name-unaligned", base64url(new Uint8Array(NAME_NONCE_LEN + NAME_BLOCK + 1 + TAG_LEN)), "identity"],
		["name-forged", base64url(forged), "authentication"],
		["name-other-epoch", (await deriveName(gen(OTHER_EPOCH), utf8("video"))).physicalName, "authentication"],
		// Sealed under the name key but not its HMAC nonce: a second physical name for "video".
		["name-nonce-mismatch", await sealName(video, video.padded, otherNonce), "authentication"],
		["name-padding-overlong", await sealName(video, overlong), "authentication"],
		["name-padding-unmarked", await sealName(video, unmarked), "authentication"],
		["name-not-utf8", await sealName(video, pad(new Uint8Array([0xff]))), "authentication"],
	] as const) {
		negative.push({ id, operation: "name", error, generation: generationJson(base), physical_name: physicalName });
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
		semantic_name: "video",
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
			path_label: hex(PATH_LABEL),
			name_mac_label: hex(NAME_MAC_LABEL),
			name_key_label: hex(NAME_KEY_LABEL),
			key_label: hex(KEY_LABEL),
			domain_group: DOMAIN_GROUP,
			domain_datagram: DOMAIN_DATAGRAM,
			tag_len: TAG_LEN,
			key_len: KEY_LEN,
			path_len: PATH_LEN,
			name_mac_len: NAME_MAC_LEN,
			name_nonce_len: NAME_NONCE_LEN,
			name_block: NAME_BLOCK,
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
		paths,
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
			semantic_name: string;
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
	| { operation: "name"; generation: ReturnType<typeof generationJson>; physical_name: string }
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

	for (const row of doc.paths) {
		const credential: Credential = {
			context: unhex(row.context),
			kid: BigInt(row.kid),
			secret: unhex(row.secret),
		};
		const path = await derivePath(credential, utf8(row.semantic_name));
		assertEqual("path prk", hex(path.prk), row.prk);
		assertEqual("path info", hex(path.info), row.path_info);
		assertEqual("path material", hex(path.material), row.path_material);
		assertEqual("opaque path", path.opaque, row.opaque);
	}

	for (const row of doc.derivation) {
		const generation = parseGeneration(row);
		const semantic = unhex(row.semantic_name_bytes);
		const named = await deriveName(generation, semantic);
		const keyed = await deriveKey(generation, semantic, Number(row.domain) as Domain, named.prk);
		assertEqual("prk", hex(named.prk), row.prk);
		assertEqual("name_mac_info", hex(named.macInfo), row.name_mac_info);
		assertEqual("name_mac", hex(named.mac), row.name_mac);
		assertEqual("name_key_info", hex(named.keyInfo), row.name_key_info);
		assertEqual("name_key", hex(named.key), row.name_key);
		assertEqual("padded", hex(named.padded), row.padded);
		assertEqual("name_nonce", hex(named.nonce), row.name_nonce);
		assertEqual("physical_name", named.physicalName, row.physical_name);
		assertEqual("key_info", hex(keyed.info), row.key_info);
		assertEqual("key", hex(keyed.key), row.key);
	}

	for (const row of doc.naming) {
		const generation = { ...parseGeneration(doc.generation), epoch: utf8(row.epoch) };
		const named = await deriveName(generation, unhex(row.semantic_name_bytes));
		assertEqual(`${row.id} padded`, hex(named.padded), row.padded);
		assertEqual(`${row.id} naming`, named.physicalName, row.physical_name);
		assertEqual(`${row.id} recovery`, hex(await openName(generation, row.physical_name)), row.semantic_name_bytes);
	}

	for (const row of [...doc.groups, ...doc.datagrams]) {
		const generation = parseGeneration(row);
		const semantic = utf8(String(row.semantic_name));
		const named = await deriveName(generation, semantic);
		const keyed = await deriveKey(generation, semantic, Number(row.domain) as Domain, named.prk);
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
					const keyed = await deriveKey(parseGeneration(row.generation), utf8(row.semantic_name), row.domain);
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
				case "name":
					await openName(parseGeneration(row.generation), row.physical_name);
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
		// Escape the BOM vector so it stays visible in the file.
		const json = JSON.stringify(generated, null, "\t").replaceAll("\ufeff", "\\ufeff");
		writeFileSync(jsonPath, `${json}\n`);
	}
	const onDisk = JSON.parse(readFileSync(jsonPath, "utf8")) as Awaited<ReturnType<typeof generate>>;
	await verify(onDisk);
	if (JSON.stringify(onDisk) !== JSON.stringify(generated)) {
		throw new Error("drafts/moq-e2ee-00.json is stale; run bun drafts/moq-e2ee-00.ts --write");
	}
	console.log(
		`moq-e2ee-00: ${onDisk.paths.length} path, ${onDisk.derivation.length} derivation, ${onDisk.groups.length} group, ${onDisk.datagrams.length} datagram, ${onDisk.negative.length} negative vectors`,
	);
}

if (import.meta.main) {
	await main();
}
