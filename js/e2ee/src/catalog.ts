import {
	DOMAIN_GROUP,
	MAX_GROUPED_PAYLOAD,
	SEMANTIC_HANG_CATALOG,
	SEMANTIC_HANG_CATALOG_Z,
	SEMANTIC_MSF_CATALOG,
} from "./constants.ts";
import type { Credential } from "./credential.ts";

/** Result of {@link protectCatalog}. */
export interface ProtectedCatalog {
	/** Opaque physical name derived from the semantic catalog name. */
	physicalName: string;
	/** Ciphertext || tag to publish as the catalog track payload. */
	payload: Uint8Array;
}

/**
 * Derive the catalog's physical name and encrypt its bytes.
 *
 * `plaintext` is the catalog representation after any compression. Encrypting then
 * compressing is forbidden: pass already-compressed bytes for `catalog.json.z`.
 */
export async function protectCatalog(input: {
	credential: Credential;
	semanticName: string;
	plaintext: Uint8Array;
	group?: number;
	frame?: number;
}): Promise<ProtectedCatalog> {
	const physicalName = await input.credential.opaqueName(input.semanticName);
	const payload = await input.credential.seal({
		physicalName,
		domain: DOMAIN_GROUP,
		group: input.group ?? 0,
		frame: input.frame ?? 0,
		plaintext: input.plaintext,
		payloadLimit: MAX_GROUPED_PAYLOAD,
	});
	return { physicalName, payload };
}

/** Derive the catalog's physical name and decrypt a payload published under it. */
export async function openCatalog(input: {
	credential: Credential;
	semanticName: string;
	payload: Uint8Array;
	group?: number;
	frame?: number;
}): Promise<Uint8Array> {
	const physicalName = await input.credential.opaqueName(input.semanticName);
	return input.credential.open({
		physicalName,
		domain: DOMAIN_GROUP,
		group: input.group ?? 0,
		frame: input.frame ?? 0,
		payload: input.payload,
		payloadLimit: MAX_GROUPED_PAYLOAD,
	});
}

/** Well-known catalog semantic names this profile encrypts. */
export const CatalogName = {
	/** Hang `catalog.json`. */
	hang: SEMANTIC_HANG_CATALOG,
	/** Hang `catalog.json.z` (compress, then encrypt). */
	hangCompressed: SEMANTIC_HANG_CATALOG_Z,
	/** MSF `catalog`. */
	msf: SEMANTIC_MSF_CATALOG,
} as const;
