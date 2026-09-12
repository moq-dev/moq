/**
 * Text chat over a MoQ track, matching iroh-live's `iroh-rooms` and `moq-room::chat`.
 *
 * A broadcast carries chat on one well-known track, {@link TRACK}. Each message
 * is a single group holding one frame of UTF-8 text. The sender's identity
 * comes from the broadcast that carries the track, not from the payload.
 *
 * This is not hang.live's `hang/chat.json` snapshot; that stays an app-defined
 * catalog extension (`TRACK.chat` in the metadata module). This track is an
 * ordered append-log.
 *
 * @module
 */

import type { Broadcast, Track } from "@moq/net";

/** Name of the track that carries chat messages. */
export const TRACK = "chat";

/**
 * Publisher tie-break priority for the chat track.
 *
 * Lower than audio and video, which are the tracks a viewer notices first when
 * the link is congested.
 */
export const PRIORITY = 10;

/** Track settings for the chat track: ordered, because chat is read oldest first. */
export function info(): Pick<Track.Info, "priority" | "ordered"> {
	return { priority: PRIORITY, ordered: true };
}

/** A received chat message, with the time it arrived. */
export type Message = {
	/** The message text. */
	text: string;
	/** When this message was received locally. */
	receivedAt: Date;
};

/** Writer half of a chat track. */
export class Publisher {
	#track: Track.Producer;

	/** Creates the chat track on `broadcast` and returns a publisher for it. */
	static create(broadcast: Broadcast.Producer): Publisher {
		return new Publisher(broadcast.createTrack(TRACK, info()));
	}

	/** Creates a publisher over an existing track producer. */
	constructor(track: Track.Producer) {
		this.#track = track;
	}

	/**
	 * Sends a text message on the chat track.
	 *
	 * Empty messages are dropped rather than written, because a subscriber
	 * cannot tell them apart from a group it failed to read.
	 */
	send(text: string): void {
		if (text === "") return;
		this.#track.writeString(text);
	}

	/**
	 * Ends the chat track so subscribers drain already-sent messages, then see close.
	 *
	 * Dropping a publisher without this first is an abrupt teardown.
	 */
	finish(): void {
		this.#track.close();
	}
}

/** Reader half of a chat track. */
export class Subscriber {
	#track: Track.Subscriber;

	/** Subscribes to the chat track of `broadcast`. */
	static subscribe(broadcast: Broadcast.Consumer): Subscriber {
		return new Subscriber(broadcast.track(TRACK).subscribe({ ordered: true }));
	}

	/** Creates a subscriber over an existing track subscriber. */
	constructor(track: Track.Subscriber) {
		this.#track = track;
	}

	/**
	 * Waits for the next chat message.
	 *
	 * Returns `undefined` once the track ends.
	 */
	async recv(): Promise<Message | undefined> {
		for (;;) {
			const text = await this.#track.readString();
			if (text === undefined) return undefined;
			if (text === "") continue;
			return { text, receivedAt: new Date() };
		}
	}
}
