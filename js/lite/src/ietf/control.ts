import { Mutex } from "async-mutex";
import type { Reader, Stream as StreamInner, Writer } from "../stream.ts";
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
import { Subscribe, SubscribeError, SubscribeOk, Unsubscribe } from "./subscribe.ts";
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

type V14MessageType = (typeof MessagesV14)[keyof typeof MessagesV14];
type V15MessageType = (typeof MessagesV15)[keyof typeof MessagesV15];
type MessageType = V14MessageType | V15MessageType;

// Type for control message instances (not constructors)
export type Message = InstanceType<MessageType>;

export class Stream {
	stream: StreamInner;
	version: IetfVersion;

	// The client always starts at 0.
	#requestId = 0n;

	#maxRequestId: bigint;

	#maxRequestIdPromise?: Promise<void>; // unblocks when there's a new max request id
	#maxRequestIdResolve!: () => void; // resolves the max request id promise

	#writeLock = new Mutex();
	#readLock = new Mutex();

	constructor(stream: StreamInner, maxRequestId: bigint, version: IetfVersion = Version.DRAFT_14) {
		this.stream = stream;
		this.version = version;
		this.#maxRequestId = maxRequestId;
		this.#maxRequestIdPromise = new Promise((resolve) => {
			this.#maxRequestIdResolve = resolve;
		});
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
			// Extra version arg is silently ignored by messages that don't need it
			await (message.encode as (w: Writer, v?: IetfVersion) => Promise<void>)(this.stream.writer, this.version);
		});
	}

	/**
	 * Reads a control message from the control stream.
	 * Returns the message type and a reader for the payload.
	 */
	async read(): Promise<Message> {
		return await this.#readLock.runExclusive(async () => {
			const messageType = await this.stream.reader.u53();

			const messages = this.version === Version.DRAFT_15 ? MessagesV15 : MessagesV14;
			if (!(messageType in messages)) {
				throw new Error(`Unknown control message type: ${messageType}`);
			}

			try {
				const msgClass = messages[messageType as keyof typeof messages];

				// Extra version arg is silently ignored by messages that don't need it
				const msg = await (msgClass as { decode: (r: Reader, v?: IetfVersion) => Promise<Message> }).decode(
					this.stream.reader,
					this.version,
				);

				console.debug("message read", msg);
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
		this.#maxRequestIdResolve();
		this.#maxRequestIdPromise = new Promise((resolve) => {
			this.#maxRequestIdResolve = resolve;
		});
	}

	async nextRequestId(): Promise<bigint | undefined> {
		while (true) {
			const id = this.#requestId;
			if (id < this.#maxRequestId) {
				this.#requestId += 2n;
				return id;
			}

			if (!this.#maxRequestIdPromise) {
				return undefined;
			}

			console.warn("blocked on max request id");
			await this.#maxRequestIdPromise;
		}
	}

	close(): void {
		this.#maxRequestIdResolve();
		this.#maxRequestIdPromise = undefined;
	}
}
