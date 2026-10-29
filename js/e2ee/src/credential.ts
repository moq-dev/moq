import { type Epoch, Path } from "@moq/net";
import { checkBytes, checkU53, encode, pathInfo } from "./derive.ts";
import { Failure } from "./failure.ts";
import type * as Generation from "./generation.ts";
import { makeConsumer, makeProducer } from "./generation-internal.ts";
import { PATH_LEN } from "./limits.ts";
import { bind, expand } from "./secret.ts";

interface ConfigType {
	/** Opaque bytes both ends agree on as the broadcast's end-to-end identity. */
	context: Uint8Array;
	/** Selects among the credentials the application retains; rotating the secret is a new kid. */
	kid: number;
	/**
	 * 32 bytes from a cryptographically secure random generator, or a nonextractable HKDF
	 * `CryptoKey` imported from them with `deriveBits` usage. Bytes are copied into a
	 * nonextractable key at construction; the caller may wipe its buffer afterwards.
	 */
	secret: Uint8Array | CryptoKey;
}

/**
 * An immutable broadcast credential. It holds the secret only as a nonextractable
 * `CryptoKey` outside the object, so it never serializes or prints it.
 */
export class Credential {
	/** Key identifier. */
	readonly kid: number;

	#context: Uint8Array;

	/**
	 * Check the bounds and take the secret.
	 *
	 * Throws {@link Failure} `identity` if `context` exceeds 65535 bytes or `kid` is not an
	 * integer in `0..=2^53-1`, and `invalid_secret` if the secret is not 32 bytes or not a
	 * nonextractable HKDF key with `deriveBits` usage.
	 */
	constructor(config: Credential.Config) {
		checkBytes(config.context.byteLength);
		checkU53(config.kid);
		this.#context = config.context.slice();
		this.kid = config.kid;
		bind(this, config.secret);
	}

	/** Broadcast context bytes, copied so the credential stays immutable. */
	get context(): Uint8Array {
		return this.#context.slice();
	}

	/** The opaque broadcast path for a semantic broadcast name; each instance announces its epoch on the route. */
	async path(semantic: string): Promise<Path.Valid> {
		const material = await expand(this, pathInfo(this.context, this.kid, semantic), PATH_LEN);
		return Path.from(encode(material));
	}

	/** Mint a fresh epoch and a producing generation. */
	produce(): Generation.Producer {
		return makeProducer(this);
	}

	/** Bind a discovered epoch as a consume-only generation; throws {@link Failure} `identity` if it is not canonical. */
	consume(epoch: Epoch.Valid): Generation.Consumer {
		return makeConsumer(this, epoch);
	}
}

/** Types on {@link Credential}: the handle is the class, these are its associated types. */
export namespace Credential {
	/** What the application distributes over its own authenticated channel. */
	export type Config = ConfigType;
}
