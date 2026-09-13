import type { Group, Time } from "@moq/net";
import { DOMAIN_GROUP, MAX_GROUPED_PAYLOAD, TAG_LEN } from "./constants.ts";
import type { Credential } from "./credential.ts";
import { Failure } from "./error.ts";
import type { Pump } from "./pump.ts";
import type { GroupWindow } from "./window.ts";

/** A sealed frame ready to retransmit without encrypting again. */
export type SealedFrame = {
	payload: Uint8Array;
	timestamp: Time.Timestamp;
};

/**
 * Write side of a protected group. Frame indices are assigned in call order and bound
 * into the nonce; {@link writeFrame} is async because WebCrypto is async.
 */
export class GroupProducer {
	/** Sequence number of this group within its track. */
	readonly sequence: number;

	#inner: Group.Producer;
	#credential: Credential;
	#physicalName: string;
	#pump: Pump;
	#nextFrame = 0;
	#sealed: SealedFrame[] = [];

	/** @internal */
	constructor(inner: Group.Producer, credential: Credential, physicalName: string, pump: Pump) {
		this.#inner = inner;
		this.#credential = credential;
		this.#physicalName = physicalName;
		this.#pump = pump;
		this.sequence = inner.sequence;
	}

	/** Encrypt `frame` as the next frame in this group and write the ciphertext. */
	async writeFrame(frame: Group.Frame): Promise<void> {
		const frameId = this.#nextFrame++;
		if (frame.payload.byteLength + TAG_LEN > MAX_GROUPED_PAYLOAD) throw new Failure("oversize");
		const ciphertext = await this.#pump.submit(() =>
			this.#credential.seal({
				physicalName: this.#physicalName,
				domain: DOMAIN_GROUP,
				group: this.sequence,
				frame: frameId,
				plaintext: frame.payload,
				payloadLimit: MAX_GROUPED_PAYLOAD,
			}),
		);
		const sealed: SealedFrame = { payload: ciphertext, timestamp: frame.timestamp };
		this.#sealed.push(sealed);
		this.#inner.writeFrame(sealed);
	}

	/** Ciphertext frames this group has produced, for retransmission. */
	sealed(): readonly SealedFrame[] {
		return this.#sealed;
	}

	/** Close the inner group, optionally aborting readers. */
	close(abort?: Error): void {
		this.#inner.close(abort);
	}

	/** Abort this group so a ciphertext retransmission can replace it. */
	abort(reason: Error = new Error("replaced")): void {
		this.#inner.close(reason);
	}

	/** The inner net group, for insertion and replacement. @internal */
	inner(): Group.Producer {
		return this.#inner;
	}
}

/**
 * Read side of a protected group. Decrypts in frame order; an authentication
 * failure is thrown and the caller ends the track.
 */
export class GroupConsumer {
	/** Sequence number of this group within its track. */
	readonly sequence: number;

	#inner: Group.Consumer;
	#credential: Credential;
	#physicalName: string;
	#pump: Pump;
	#window: GroupWindow;
	#onAuthentication: (error: Failure) => void;

	/** @internal */
	constructor(
		inner: Group.Consumer,
		credential: Credential,
		physicalName: string,
		pump: Pump,
		window: GroupWindow,
		onAuthentication: (error: Failure) => void,
	) {
		this.#inner = inner;
		this.#credential = credential;
		this.#physicalName = physicalName;
		this.#pump = pump;
		this.#window = window;
		this.#onAuthentication = onAuthentication;
		this.sequence = inner.sequence;
	}

	/** Decrypt the next frame, or `undefined` at end of group. */
	async readFrame(): Promise<Group.Frame | undefined> {
		for (;;) {
			const next = await this.#inner.readFrameSequence();
			if (!next) return undefined;
			try {
				this.#window.claim(this.sequence, next.sequence);
			} catch (error) {
				if (error instanceof Failure && error.code === "duplicate") continue;
				throw error;
			}
			try {
				const plaintext = await this.#pump.submit(() =>
					this.#credential.open({
						physicalName: this.#physicalName,
						domain: DOMAIN_GROUP,
						group: this.sequence,
						frame: next.sequence,
						payload: next.payload,
						payloadLimit: MAX_GROUPED_PAYLOAD,
					}),
				);
				return { payload: plaintext, timestamp: next.timestamp };
			} catch (error) {
				if (error instanceof Failure && error.code === "authentication") this.#onAuthentication(error);
				throw error;
			}
		}
	}

	/** Close this consumer. */
	close(abort?: Error): void {
		this.#inner.close(abort);
	}
}
