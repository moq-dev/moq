import { Mutex } from "async-mutex";
import type { Stream as StreamInner, Writer } from "../stream.ts";
import { Fetch, FetchCancel, FetchError, FetchOk } from "./fetch.ts";
import { GoAway } from "./goaway.ts";
import { Publish, PublishDone, PublishError, PublishOk } from "./publish.ts";
import {
	PublishNamespace,
	PublishNamespaceCancel,
	PublishNamespaceDone,
	PublishNamespaceError,
	PublishNamespaceOk,
} from "./publish_namespace.ts";
import { MaxRequestId, RequestError, RequestOk, RequestsBlocked } from "./request.ts";
import * as Setup from "./setup.ts";
import { Subscribe, SubscribeError, SubscribeOk, SubscribeUpdate, Unsubscribe } from "./subscribe.ts";
import {
	SubscribeNamespace,
	SubscribeNamespaceError,
	SubscribeNamespaceOk,
	UnsubscribeNamespace,
} from "./subscribe_namespace.ts";
import { TrackStatus, TrackStatusRequest } from "./track.ts";
import { type IetfVersion, Version } from "./version.ts";

// v14 message map — IDs that have different meanings in v15 are handled specially
const MessagesV14 = {
	[Setup.ClientSetup.id]: Setup.ClientSetup,
	[Setup.ServerSetup.id]: Setup.ServerSetup,
	[SubscribeUpdate.id]: SubscribeUpdate,
	[Subscribe.id]: Subscribe,
	[SubscribeOk.id]: SubscribeOk,
	[SubscribeError.id]: SubscribeError,
	[PublishNamespace.id]: PublishNamespace,
	[PublishNamespaceOk.id]: PublishNamespaceOk,
	[PublishNamespaceError.id]: PublishNamespaceError,
	[PublishNamespaceDone.id]: PublishNamespaceDone,
	[Unsubscribe.id]: Unsubscribe,
	[PublishDone.id]: PublishDone,
	[PublishNamespaceCancel.id]: PublishNamespaceCancel,
	[TrackStatusRequest.id]: TrackStatusRequest,
	[TrackStatus.id]: TrackStatus,
	[GoAway.id]: GoAway,
	[Fetch.id]: Fetch,
	[FetchCancel.id]: FetchCancel,
	[FetchOk.id]: FetchOk,
	[FetchError.id]: FetchError,
	[SubscribeNamespace.id]: SubscribeNamespace,
	[SubscribeNamespaceOk.id]: SubscribeNamespaceOk,
	[SubscribeNamespaceError.id]: SubscribeNamespaceError,
	[UnsubscribeNamespace.id]: UnsubscribeNamespace,
	[Publish.id]: Publish,
	[PublishOk.id]: PublishOk,
	[PublishError.id]: PublishError,
	[MaxRequestId.id]: MaxRequestId,
	[RequestsBlocked.id]: RequestsBlocked,
} as const;

// v15 message map — 0x05 → RequestError, 0x07 → RequestOk (different wire format)
// Messages removed in v15 (0x08, 0x0E, 0x12, 0x13, 0x19, 0x1E, 0x1F) are excluded and will be rejected
const MessagesV15 = {
	[Setup.ClientSetup.id]: Setup.ClientSetup,
	[Setup.ServerSetup.id]: Setup.ServerSetup,
	[SubscribeUpdate.id]: SubscribeUpdate,
	[Subscribe.id]: Subscribe,
	[SubscribeOk.id]: SubscribeOk,
	[RequestError.id]: RequestError, // 0x05 → RequestError instead of SubscribeError
	[PublishNamespace.id]: PublishNamespace,
	[RequestOk.id]: RequestOk, // 0x07 → RequestOk instead of PublishNamespaceOk
	[PublishNamespaceDone.id]: PublishNamespaceDone,
	[Unsubscribe.id]: Unsubscribe,
	[PublishDone.id]: PublishDone,
	[PublishNamespaceCancel.id]: PublishNamespaceCancel,
	[TrackStatusRequest.id]: TrackStatusRequest,
	[GoAway.id]: GoAway,
	[Fetch.id]: Fetch,
	[FetchCancel.id]: FetchCancel,
	[FetchOk.id]: FetchOk,
	[SubscribeNamespace.id]: SubscribeNamespace,
	[UnsubscribeNamespace.id]: UnsubscribeNamespace,
	[Publish.id]: Publish,
	[MaxRequestId.id]: MaxRequestId,
	[RequestsBlocked.id]: RequestsBlocked,
} as const;

// v16 message map — SubscribeNamespace (0x11) and UnsubscribeNamespace (0x14) move to bidi streams
const MessagesV16 = {
	[Setup.ClientSetup.id]: Setup.ClientSetup,
	[Setup.ServerSetup.id]: Setup.ServerSetup,
	[SubscribeUpdate.id]: SubscribeUpdate,
	[Subscribe.id]: Subscribe,
	[SubscribeOk.id]: SubscribeOk,
	[RequestError.id]: RequestError, // 0x05 → RequestError
	[PublishNamespace.id]: PublishNamespace,
	[RequestOk.id]: RequestOk, // 0x07 → RequestOk
	[PublishNamespaceDone.id]: PublishNamespaceDone,
	[Unsubscribe.id]: Unsubscribe,
	[PublishDone.id]: PublishDone,
	[PublishNamespaceCancel.id]: PublishNamespaceCancel,
	[TrackStatusRequest.id]: TrackStatusRequest,
	[GoAway.id]: GoAway,
	[Fetch.id]: Fetch,
	[FetchCancel.id]: FetchCancel,
	[FetchOk.id]: FetchOk,
	// SubscribeNamespace (0x11) removed — now on bidi stream
	// UnsubscribeNamespace (0x14) removed — now use stream close
	[Publish.id]: Publish,
	[MaxRequestId.id]: MaxRequestId,
	[RequestsBlocked.id]: RequestsBlocked,
} as const;

// v17 message map — uses unified Setup (0x2F00), removes several messages
const MessagesV17 = {
	[Setup.Setup.id]: Setup.Setup, // 0x2F00: unified SETUP
	[SubscribeUpdate.id]: SubscribeUpdate, // 0x02: REQUEST_UPDATE
	[Subscribe.id]: Subscribe,
	[SubscribeOk.id]: SubscribeOk,
	[RequestError.id]: RequestError, // 0x05
	[PublishNamespace.id]: PublishNamespace,
	[RequestOk.id]: RequestOk, // 0x07
	// 0x08: NAMESPACE (bidi stream only)
	// 0x09: removed in d17
	// 0x0a: removed in d17
	[PublishDone.id]: PublishDone,
	// 0x0c: removed in d17
	[TrackStatusRequest.id]: TrackStatusRequest,
	// 0x0e: NAMESPACE_DONE (bidi stream only), NOT TrackStatus
	// 0x0f: PUBLISH_BLOCKED (bidi stream only)
	[GoAway.id]: GoAway,
	[Fetch.id]: Fetch,
	// FetchCancel (0x17) removed in d17
	[FetchOk.id]: FetchOk,
	[SubscribeNamespace.id]: SubscribeNamespace,
	[Publish.id]: Publish,
	// MaxRequestId (0x15) removed in d17
	// RequestsBlocked (0x1a) removed in d17
} as const;

type V14MessageType = (typeof MessagesV14)[keyof typeof MessagesV14];
type V15MessageType = (typeof MessagesV15)[keyof typeof MessagesV15];
type V16MessageType = (typeof MessagesV16)[keyof typeof MessagesV16];
type V17MessageType = (typeof MessagesV17)[keyof typeof MessagesV17];
type MessageType = V14MessageType | V15MessageType | V16MessageType | V17MessageType;

// Type for control message instances (not constructors)
export type Message = InstanceType<MessageType>;

export class Stream {
	stream: StreamInner;
	version: IetfVersion;

	// The client always starts at 0.
	#requestId = 0n;

	#maxRequestId: bigint;

	#maxRequestIdUpdate?: PromiseWithResolvers<void>;

	#writeLock = new Mutex();
	#readLock = new Mutex();

	constructor({
		stream,
		maxRequestId,
		version = Version.DRAFT_14,
	}: {
		stream: StreamInner;
		maxRequestId: bigint;
		version?: IetfVersion;
	}) {
		this.stream = stream;
		this.version = version;
		// Set version on reader/writer so varint encoding is version-aware
		this.stream.reader.version = version;
		this.stream.writer.version = version;
		this.#maxRequestId = maxRequestId;
		this.#maxRequestIdUpdate = Promise.withResolvers();
	}

	/**
	 * Writes a control message to the control stream with proper framing.
	 * Format: Message Type (varint) + Message Length (u16) + Message Payload
	 */
	async write<T extends Message>(message: T): Promise<void> {
		console.debug("message write", message);

		await this.#writeLock.runExclusive(async () => {
			// Write message type
			await this.stream.writer.u53((message.constructor as MessageType).id);

			// Write message payload with u16 size prefix
			await (message.encode as (w: Writer, v: IetfVersion) => Promise<void>)(this.stream.writer, this.version);
		});
	}

	/**
	 * Reads a control message from the control stream.
	 * Returns the message type and a reader for the payload.
	 */
	async read(): Promise<Message> {
		return await this.#readLock.runExclusive(async () => {
			const messageType = await this.stream.reader.u53();

			let messages: Record<number, MessageType>;
			if (this.version === Version.DRAFT_17) {
				messages = MessagesV17 as unknown as Record<number, MessageType>;
			} else if (this.version === Version.DRAFT_16) {
				messages = MessagesV16 as unknown as Record<number, MessageType>;
			} else if (this.version === Version.DRAFT_15) {
				messages = MessagesV15 as unknown as Record<number, MessageType>;
			} else {
				messages = MessagesV14 as unknown as Record<number, MessageType>;
			}

			if (!(messageType in messages)) {
				throw new Error(`Unknown control message type: ${messageType}`);
			}

			try {
				const msgClass = messages[messageType];
				const msg = await msgClass.decode(this.stream.reader, this.version);
				return msg;
			} catch (err) {
				console.error("failed to decode message", messageType, err);
				throw err;
			}
		});
	}

	maxRequestId(max: bigint): void {
		if (max <= this.#maxRequestId) {
			throw new Error(
				`max request id must be greater than current max request id: max=${max} current=${this.#maxRequestId}`,
			);
		}

		this.#maxRequestId = max;
		this.#maxRequestIdUpdate?.resolve();
		this.#maxRequestIdUpdate = Promise.withResolvers();
	}

	async nextRequestId(): Promise<bigint | undefined> {
		while (true) {
			const id = this.#requestId;

			// d17: no flow control, always allowed
			if (this.version === Version.DRAFT_17) {
				this.#requestId += 2n;
				return id;
			}

			if (id < this.#maxRequestId) {
				this.#requestId += 2n;
				return id;
			}

			if (!this.#maxRequestIdUpdate) {
				return undefined;
			}

			console.warn("blocked on max request id");
			await this.#maxRequestIdUpdate.promise;
		}
	}

	close(): void {
		this.#maxRequestIdUpdate?.resolve();
		this.#maxRequestIdUpdate = undefined;
	}
}
