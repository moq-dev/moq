import * as z from "zod/mini";
import { decodeInitSegment } from "../cmaf/decode";
import type { Container } from "./container";

/**
 * Well-known DRM system IDs, as draft-ietf-moq-cmsf lists them.
 */
export const DRM_SYSTEM = {
	widevine: "edef8ba9-79d6-4ace-a3c8-27dcd51d21ed",
	playready: "9a04f079-9840-4286-ab92-e65be0885f95",
	fairplay: "94ce86fb-07ff-4f43-adb8-93d2fa968ca2",
	// The W3C Common System ID: for testing, since the key travels in the clear.
	clearkey: "1077efec-c0b2-4d02-ace3-3c1e52e2fb4b",
} as const;

/** A license or certificate server. */
export const DrmServerSchema = z.object({
	url: z.url(),
	// The protocol a license server speaks (e.g. `EME-1.0`), or the MIME type of a certificate.
	type: z.optional(z.string()),
});

/** A license or certificate server. */
export type DrmServer = z.infer<typeof DrmServerSchema>;

/**
 * One DRM system protecting renditions under one Common Encryption scheme and one default key.
 *
 * Each protected video or audio rendition lists the IDs of its entries in its own `drm` field;
 * each entry maps onto one of draft-ietf-moq-cmsf's `contentProtections`. A CMSF entry naming
 * several default keys has no hang form, so it reads as one this build cannot read.
 */
export const DrmSystemSchema = z.object({
	// The DRM system ID, as a UUID string (see `DRM_SYSTEM`).
	system: z.guid(),
	// How the samples are encrypted: draft-ietf-moq-cmsf allows `cenc` and `cbcs`, and another
	// is kept so a scheme a later draft adds leaves the rest of the catalog readable.
	scheme: z.string(),
	// The default key ID (`tenc`'s `default_KID`), as a UUID string.
	kid: z.guid(),
	// The system's whole `pssh` box from the init segment, base64.
	pssh: z.optional(z.base64()),
	// Where to request a license.
	license: z.optional(DrmServerSchema),
	// Where to fetch the server certificate, which systems such as FairPlay need first.
	certificate: z.optional(DrmServerSchema),
});

/** One DRM system protecting renditions. */
export type DrmSystem = z.infer<typeof DrmSystemSchema>;

/**
 * A root `drm` entry: one this build reads, or one it cannot (such as a malformed UUID or URL),
 * kept as-is. A rendition listing either is protected.
 */
export const DrmEntrySchema = z.union([DrmSystemSchema, z.record(z.string(), z.unknown())]);

/** A root `drm` entry. */
export type DrmEntry = z.infer<typeof DrmEntrySchema>;

/** A protected rendition lists a `drm` entry the root `drm` section lacks. */
export class MissingDrm extends Error {
	/** The rendition listing the entry. */
	readonly rendition: string;
	/** The entry ID the root lacks. */
	readonly id: string;

	constructor(rendition: string, id: string) {
		super(`rendition ${rendition} references missing drm entry ${id}`);
		this.name = "MissingDrm";
		this.rendition = rendition;
		this.id = id;
	}
}

/**
 * Whether a rendition is protected, so its samples are encrypted and only a consumer that can
 * decrypt them may select it: it lists `drm` entries, or it is CMAF and its init holds an encrypted
 * (`encv`/`enca`) sample entry, so a publisher that encrypts without signalling it is never read as
 * clear. An init that fails to parse is not one, since no consumer reads it either way.
 */
export function isProtected(config: { drm?: string[]; container?: Container }): boolean {
	if ((config.drm?.length ?? 0) > 0) return true;
	if (config.container?.kind !== "cmaf") return false;
	try {
		const init = Uint8Array.from(atob(config.container.init), (char) => char.charCodeAt(0));
		return decodeInitSegment(init).encrypted;
	} catch {
		return false;
	}
}
