/**
 * Route key proofs: an endpoint shows its peer that it holds an Ed25519 key, bound to the
 * session by keying material both ends export from its handshake.
 *
 * A tab proves one key per page load, so a peer recognizes it across its sessions. Only a
 * transport that exposes `exportKeyingMaterial` can bind a proof; elsewhere the tab sends
 * none and stays unproven.
 *
 * @module
 */

const EXPORTER_LABEL = new TextEncoder().encode("EXPORTER-moq-lite-route-key");
const DOMAIN = new TextEncoder().encode("moq-lite route key");
const SECRET_SIZE = 32;

/** The keying material export a WebTransport may offer; only some browsers ship it. */
type Exporter = (label: BufferSource, context: BufferSource, outputLength: number) => Promise<ArrayBuffer | Uint8Array>;

let pageKey: Promise<CryptoKeyPair> | undefined;

/**
 * The proof to send in SETUP for `transport`, or `undefined` when it cannot export keying
 * material. `client` is whether this endpoint dialed the session.
 */
export async function proveRouteKey(transport: WebTransport, client: boolean): Promise<Uint8Array | undefined> {
	const exporter = (transport as { exportKeyingMaterial?: Exporter }).exportKeyingMaterial;
	if (typeof exporter !== "function") return undefined;
	const secret = new Uint8Array(await exporter.call(transport, EXPORTER_LABEL, new Uint8Array(), SECRET_SIZE));
	pageKey ??= crypto.subtle.generateKey({ name: "Ed25519" }, false, ["sign", "verify"]) as Promise<CryptoKeyPair>;
	return sign(await pageKey, secret, client);
}

/** Sign the proof for `secret`, the session's exported keying material. Exported for tests. */
export async function sign(key: CryptoKeyPair, secret: Uint8Array, client: boolean): Promise<Uint8Array> {
	if (secret.byteLength !== SECRET_SIZE) throw new Error("route key secret must be 32 bytes");
	// The domain, then the prover's side of the session (0 dialed, 1 accepted), then the secret.
	const message = new Uint8Array(DOMAIN.byteLength + 1 + SECRET_SIZE);
	message.set(DOMAIN);
	message[DOMAIN.byteLength] = client ? 0 : 1;
	message.set(secret, DOMAIN.byteLength + 1);

	const publicKey = new Uint8Array(await crypto.subtle.exportKey("raw", key.publicKey));
	const signature = new Uint8Array(await crypto.subtle.sign({ name: "Ed25519" }, key.privateKey, message));
	const proof = new Uint8Array(publicKey.byteLength + signature.byteLength);
	proof.set(publicKey);
	proof.set(signature, publicKey.byteLength);
	return proof;
}
