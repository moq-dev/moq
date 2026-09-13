import type { Domain } from "./constants.ts";
import type { Credential } from "./credential.ts";

/** Derive the opaque physical track name for `semanticName`. */
export function opaqueName(credential: Credential, semanticName: string): Promise<string> {
	return credential.opaqueName(semanticName);
}

/** Encrypt `plaintext` at an explicit object identity. */
export function protect(
	credential: Credential,
	input: {
		physicalName: string;
		domain: Domain;
		group: number;
		frame: number;
		plaintext: Uint8Array;
		payloadLimit: number;
	},
): Promise<Uint8Array> {
	return credential.seal(input);
}

/** Decrypt `payload` at an explicit object identity. */
export function open(
	credential: Credential,
	input: {
		physicalName: string;
		domain: Domain;
		group: number;
		frame: number;
		payload: Uint8Array;
		payloadLimit: number;
	},
): Promise<Uint8Array> {
	return credential.open(input);
}
