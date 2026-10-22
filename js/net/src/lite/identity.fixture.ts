/** Unactivated identity proposal, excluded from published builds and consumed only by adversarial tests. @module */
export interface Vectors {
	keys: { private: string; public: string; jwk: JsonWebKey }[];
	identity: string;
	cnf: string;
	message: string;
	webcrypto_signature: string;
	rust_signature: string;
}
const text = new TextEncoder();
const ID_DOMAIN = text.encode("moq-routing-identity-v1\0");
const PROOF_DOMAIN = text.encode("moq-lite-07-wip identity-proof-v1\0");
const MAC_DOMAIN = text.encode("moq-lite-07-wip own-control-candidate-v1\0");
type Bytes = Uint8Array<ArrayBuffer>;
function concat(...parts: Uint8Array[]): Bytes {
	const bytes = new Uint8Array(parts.reduce((length, part) => length + part.length, 0));
	let offset = 0;
	for (const part of parts) {
		bytes.set(part, offset);
		offset += part.length;
	}
	return bytes;
}
function hex(bytes: Uint8Array): string {
	return Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
}
function unhex(value: string): Bytes {
	if (value.length % 2 || !/^[0-9a-f]*$/.test(value)) throw new Error("invalid fixture hex");
	return Uint8Array.from(value.match(/../g) ?? [], (part) => Number.parseInt(part, 16));
}
function check(value: unknown, reason: string): asserts value {
	if (!value) throw new Error(reason);
}
async function rejects(run: () => Promise<unknown>, reason: string) {
	let refused = false;
	try {
		await run();
	} catch {
		refused = true;
	}
	check(refused, reason);
}
const ECDSA = { name: "ECDSA", namedCurve: "P-256" };
const SIGN = { name: "ECDSA", hash: "SHA-256" };
let processKey: Promise<CryptoKeyPair> | undefined;
function process(): Promise<CryptoKeyPair> {
	// Reserve synchronously so concurrent connections share even the pending generation.
	processKey ??= crypto.subtle.generateKey(ECDSA, false, ["sign", "verify"]);
	return processKey;
}
async function imported(publicKey: Bytes): Promise<CryptoKey> {
	check(publicKey.length === 65 && publicKey[0] === 4, "canonical uncompressed P-256 key required");
	return crypto.subtle.importKey("raw", publicKey, ECDSA, false, ["verify"]);
}
async function identity(publicKey: Bytes): Promise<Bytes> {
	await imported(publicKey);
	return new Uint8Array(await crypto.subtle.digest("SHA-256", concat(ID_DOMAIN, publicKey)));
}
async function sign(key: CryptoKey, message: Bytes): Promise<Bytes> {
	const signature = new Uint8Array(await crypto.subtle.sign(SIGN, key, message));
	check(signature.length === 64, "P1363 signature width");
	return signature;
}
async function verify(publicKey: Bytes, message: Bytes, signature: Bytes): Promise<boolean> {
	if (signature.length !== 64) return false;
	return crypto.subtle.verify(SIGN, await imported(publicKey), signature, message);
}
function cnf(publicKey: Bytes, nonce: Bytes): Bytes {
	check(publicKey.length === 65 && nonce.length === 32, "fixed AUTH_CNF fields");
	return concat(Uint8Array.of(2, 97), publicKey, nonce);
}
async function decodeCnf(bytes: Bytes): Promise<Bytes> {
	check(bytes.length === 99 && bytes[0] === 2 && bytes[1] === 97, "fixed AUTH_CNF length or type");
	const publicKey = bytes.slice(2, 67);
	await imported(publicKey);
	return publicKey;
}
function transcript(a: Bytes, b: Bytes, role: number, binding?: Bytes): Bytes {
	check(a.length === 99 && b.length === 99 && (role === 0 || role === 1), "proof transcript shape");
	if (binding) check(binding.length === 32, "fixed binding width");
	return concat(
		PROOF_DOMAIN,
		Uint8Array.of(role, binding ? 1 : 0),
		a.slice(2),
		b.slice(2),
		binding ?? new Uint8Array(),
	);
}
class Slot {
	#state: "empty" | "checking" | "verified" | "failed" = "empty";
	#checks = 0;
	#owned?: string;
	#local: Bytes;
	#binding?: Bytes;
	#role: 0 | 1;
	constructor(local: Bytes, role: 0 | 1, binding?: Bytes) {
		this.#role = role;
		this.#local = local.slice();
		this.#binding = binding?.slice();
	}
	get checks(): number {
		return this.#checks;
	}
	async accept(cnf: Bytes, claimed: string, signature: Bytes): Promise<void> {
		check(this.#state === "empty", "duplicate session proof");
		this.#state = "checking";
		try {
			check(cnf.length === 99 && signature.length === 64, "fixed proof fields");
			const peer = cnf.slice(),
				proof = signature.slice();
			this.#checks++;
			const publicKey = await decodeCnf(peer);
			const derived = hex(await identity(publicKey));
			check(derived === claimed, "claimed identity differs from public key");
			const message =
				this.#role === 0
					? transcript(peer, this.#local, 0, this.#binding)
					: transcript(this.#local, peer, 1, this.#binding);
			check(await verify(publicKey, message, proof), "invalid proof");
			this.#owned = derived;
			this.#state = "verified";
		} catch (error) {
			this.#state = "failed";
			throw error;
		}
	}
	owns(claimed: string): boolean {
		return this.#state === "verified" && this.#owned === claimed;
	}
}

function u64(value: bigint): Bytes {
	check(value >= 0n && value <= 0xffffffffffffffffn, "u64 range");
	const bytes = new Uint8Array(8);
	new DataView(bytes.buffer).setBigUint64(0, value);
	return bytes;
}
// One ordered direction, not a production route format. MAC covers opaque control bytes too.
function control(counter: bigint, seqno: bigint, owned: Bytes, payload: Bytes): Bytes {
	return concat(MAC_DOMAIN, u64(counter), u64(seqno), owned, u64(BigInt(payload.length)), payload);
}
async function directional(shared: ArrayBuffer, transcript: Bytes, direction: number): Promise<CryptoKey> {
	const key = await crypto.subtle.importKey("raw", shared, "HKDF", false, ["deriveKey"]);
	return crypto.subtle.deriveKey(
		{ name: "HKDF", hash: "SHA-256", salt: transcript, info: concat(MAC_DOMAIN, Uint8Array.of(direction)) },
		key,
		{ name: "HMAC", hash: "SHA-256", length: 256 },
		false,
		["sign", "verify"],
	);
}
class Ordered {
	#next: bigint;
	#checking = false;
	#key: CryptoKey;
	constructor(key: CryptoKey, next = 0n) {
		this.#key = key;
		this.#next = next;
	}
	async receive(frame: Bytes, tag: Bytes): Promise<boolean> {
		// One verifier owns the ordered receiver, including while WebCrypto yields.
		if (this.#checking) return false;
		// Snapshot inputs before awaiting; the counter exists only inside the MACed frame.
		frame = frame.slice();
		tag = tag.slice();
		if (frame.length < MAC_DOMAIN.length + 56 || !MAC_DOMAIN.every((byte, index) => frame[index] === byte))
			return false;
		const header = new DataView(frame.buffer, frame.byteOffset + MAC_DOMAIN.length);
		const counter = header.getBigUint64(0);
		if (header.getBigUint64(48) !== BigInt(frame.length - MAC_DOMAIN.length - 56)) return false;
		if (counter !== this.#next || this.#next === 0xffffffffffffffffn) return false;
		this.#checking = true;
		try {
			if (!(await crypto.subtle.verify("HMAC", this.#key, tag, frame))) return false;
			this.#next++;
			return true;
		} finally {
			this.#checking = false;
		}
	}
}

/** Run the same private crypto/attack proposal under Bun and real browser WebCrypto. */
export async function runIdentityCases(vectors: Vectors): Promise<string[]> {
	const passed: string[] = [];
	const a = unhex(vectors.keys[0].public),
		b = unhex(vectors.keys[1].public);
	const ka = await crypto.subtle.importKey("jwk", vectors.keys[0].jwk, ECDSA, false, ["sign"]);
	const kb = await crypto.subtle.importKey("jwk", vectors.keys[1].jwk, ECDSA, false, ["sign"]);
	const ah = cnf(a, new Uint8Array(32).fill(1)),
		bh = cnf(b, new Uint8Array(32).fill(2));
	const binding = new Uint8Array(32).fill(3);
	const message = transcript(ah, bh, 0, binding);
	check(
		hex(await identity(a)) === vectors.identity && (await identity(a)).length === 32,
		"full-width identity parity",
	);
	check(hex(ah) === vectors.cnf && hex(message) === vectors.message, "wire/transcript parity");
	check(await verify(a, message, unhex(vectors.rust_signature)), "Rust signature interoperability");
	check(await verify(a, message, unhex(vectors.webcrypto_signature)), "WebCrypto signature interoperability");
	check(await verify(a, message, await sign(ka, message)), "WebCrypto signing");
	passed.push("Rust/WebCrypto key, identity, CNF, transcript, P1363 signature parity");
	const [one, two] = await Promise.all([process(), process()]);
	check(one === two && !one.privateKey.extractable, "one nonextractable tab key including pending generation");
	const restarted = await crypto.subtle.generateKey(ECDSA, false, ["sign", "verify"]);
	check(
		hex(new Uint8Array(await crypto.subtle.exportKey("raw", one.publicKey))) !==
			hex(new Uint8Array(await crypto.subtle.exportKey("raw", restarted.publicKey))),
		"restart identity",
	);
	await rejects(() => crypto.subtle.exportKey("pkcs8", one.privateKey), "private key must not export");
	passed.push("shared per-tab generation, fresh restart, nonextractable private key");
	const signature = await sign(ka, message);
	check(
		!(await verify(a, transcript(cnf(a, new Uint8Array(32).fill(4)), bh, 0, binding), signature)),
		"cross-session nonce replay",
	);
	check(!(await verify(a, transcript(ah, bh, 1, binding), signature)), "direction reflection");
	check(!(await verify(a, transcript(ah, bh, 0, new Uint8Array(32).fill(4)), signature)), "transport binding replay");
	check(!(await verify(b, message, signature)), "proof key swap");
	passed.push("nonce replay, reflection, binding replay, proof-key swap refused");
	const slot = new Slot(bh, 0, binding);
	check(!slot.owns(vectors.identity), "no-proof route race");
	const results = await Promise.allSettled([
		slot.accept(ah, vectors.identity, signature),
		slot.accept(ah, vectors.identity, signature),
	]);
	check(
		results[0].status === "fulfilled" && results[1].status === "rejected" && slot.checks === 1,
		"session-wide reservation before await",
	);
	check(slot.owns(vectors.identity) && !slot.owns(hex(await identity(b))), "verified own identity only");
	await rejects(() => slot.accept(ah, vectors.identity, signature), "token refresh must not rekey");
	const wrongId = hex(await identity(b));
	const wrong = new Slot(bh, 0, binding);
	await rejects(() => wrong.accept(ah, wrongId, signature), "claimed other identity");
	const localCopy = bh.slice(),
		bindingCopy = binding.slice(),
		peerCopy = ah.slice(),
		signatureCopy = signature.slice();
	const immutable = new Slot(localCopy, 0, bindingCopy);
	localCopy.fill(9);
	bindingCopy.fill(9);
	const pending = immutable.accept(peerCopy, vectors.identity, signatureCopy);
	peerCopy.fill(9);
	signatureCopy.fill(9);
	await pending;
	check(immutable.owns(vectors.identity), "owned transcript/input snapshots survive mutation during await");
	passed.push("no-proof/mismatched-own identity, duplicate CNF and buffer mutation races refused");
	for (const bytes of [
		ah.slice(1),
		concat(ah, Uint8Array.of(0)),
		Uint8Array.of(2, 255),
		concat(Uint8Array.of(2, 97, 2), ah.slice(3)),
	]) {
		await rejects(() => decodeCnf(bytes), "malformed fixed frame accepted");
	}
	const invalid = ah.slice();
	invalid.fill(0, 3, 67);
	await rejects(() => decodeCnf(invalid), "invalid curve point");
	check(!(await verify(a, message, new Uint8Array(64))), "zero signature scalars");
	check(!(await verify(a, message, new Uint8Array(65))), "signature length");
	passed.push("fixed lengths, canonical public key, curve point and signature scalars refused");
	// The attacker relays both honest hellos and signatures, knowing neither private key.
	const nonceOnly = transcript(ah, bh, 0);
	const relayed = await sign(ka, nonceOnly);
	const compromised = new Slot(bh, 0);
	await compromised.accept(ah, vectors.identity, relayed);
	const mutual = new Slot(ah, 1);
	await mutual.accept(bh, hex(await identity(b)), await sign(kb, transcript(ah, bh, 1)));
	check(mutual.owns(hex(await identity(b))), "mutual live-relay acceptance");
	const rebound = new Slot(bh, 0, new Uint8Array(32).fill(4));
	await rejects(() => rebound.accept(ah, vectors.identity, signature), "receiver owns its expected binding");
	const rechallenged = new Slot(cnf(b, new Uint8Array(32).fill(9)), 0, binding);
	await rejects(() => rechallenged.accept(ah, vectors.identity, signature), "receiver owns its expected challenge");
	check(compromised.owns(vectors.identity), "live relay counterexample must actually succeed");
	passed.push("keyless live whole-AUTH relay succeeds against nonce-only ownership");
	// Feasibility alternative: sign both ephemeral keys, then authenticate every own control.
	for (const sameKey of [false, true]) {
		const otherPublic = sameKey ? a : b,
			otherKey = sameKey ? ka : kb;
		const ae = await crypto.subtle.generateKey({ name: "ECDH", namedCurve: "P-256" }, false, ["deriveBits"]);
		const be = await crypto.subtle.generateKey({ name: "ECDH", namedCurve: "P-256" }, false, ["deriveBits"]);
		const me = await crypto.subtle.generateKey({ name: "ECDH", namedCurve: "P-256" }, false, ["deriveBits"]);
		const handshake = concat(
			transcript(ah, cnf(otherPublic, new Uint8Array(32).fill(2)), 0),
			new Uint8Array(await crypto.subtle.exportKey("raw", ae.publicKey)),
			new Uint8Array(await crypto.subtle.exportKey("raw", be.publicKey)),
		);
		const reverse = concat(Uint8Array.of(1), handshake);
		check(await verify(a, handshake, await sign(ka, handshake)), "signed client ephemeral");
		check(await verify(otherPublic, reverse, await sign(otherKey, reverse)), "signed server ephemeral");
		const sharedA = await crypto.subtle.deriveBits({ name: "ECDH", public: be.publicKey }, ae.privateKey, 256);
		const sharedB = await crypto.subtle.deriveBits({ name: "ECDH", public: ae.publicKey }, be.privateKey, 256);
		const sharedM = await crypto.subtle.deriveBits({ name: "ECDH", public: be.publicKey }, me.privateKey, 256);
		const send = await directional(sharedA, handshake, 0),
			receive = await directional(sharedB, handshake, 0);
		const reflected = await directional(sharedB, handshake, 1),
			forged = await directional(sharedM, handshake, 0);
		const frame = control(0n, 7n, await identity(a), text.encode("ROUTE capability-prefix"));
		const tag = new Uint8Array(await crypto.subtle.sign("HMAC", send, frame));
		const receiver = new Ordered(receive);
		check(
			!(await receiver.receive(control(0n, 8n, await identity(a), text.encode("ROUTE capability-prefix")), tag)),
			"modified seqno",
		);
		check(
			!(await receiver.receive(frame, new Uint8Array(await crypto.subtle.sign("HMAC", forged, frame)))),
			"keyless relay MAC forgery",
		);
		check(
			!(await receiver.receive(frame, new Uint8Array(await crypto.subtle.sign("HMAC", reflected, frame)))),
			"reflected direction",
		);
		const concurrent = await Promise.all([receiver.receive(frame, tag), receiver.receive(frame, tag)]);
		check(concurrent[0] && !concurrent[1], "one owner during concurrent MAC verification");
		check(!(await receiver.receive(frame, tag)), "repeated counter");
		const next = control(1n, 8n, await identity(a), text.encode("ANNOUNCE_END exact-instance"));
		check(
			await receiver.receive(next, new Uint8Array(await crypto.subtle.sign("HMAC", send, next))),
			"next authority-changing control",
		);
		// Supplying a new outer counter cannot replay old authenticated bytes: there is no such parameter.
		const changedCounter = frame.slice();
		new DataView(changedCounter.buffer).setBigUint64(MAC_DOMAIN.length, 2n);
		check(!(await receiver.receive(changedCounter, tag)), "replayed frame with rewritten counter");
		check(!(await receiver.receive(frame, tag)), "replay cannot gain a fresh outer counter");
		const exhausted = new Ordered(receive, 0xffffffffffffffffn);
		const last = control(0xffffffffffffffffn, 9n, await identity(a), text.encode("ROUTE"));
		check(
			!(await exhausted.receive(last, new Uint8Array(await crypto.subtle.sign("HMAC", send, last)))),
			"counter overflow",
		);
	}
	passed.push(
		"signed ECDH/HKDF directional MAC blocks keyless injection, seqno edits, reflection, replay and overflow, including loopback keys",
	);
	return passed;
}
