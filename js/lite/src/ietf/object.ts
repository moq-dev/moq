import type { Reader, Writer } from "../stream.ts";

const GROUP_END = 0x03;

export interface GroupFlags {
	hasExtensions: boolean;
	hasSubgroup: boolean;
	hasSubgroupObject: boolean;
	hasEnd: boolean;
	// v15: whether priority is present in the header.
	// When false (0x30 base), priority inherits from the control message.
	hasPriority: boolean;
}

/**
 * STREAM_HEADER_SUBGROUP from moq-transport spec.
 * Used for stream-per-group delivery mode.
 */
export class Group {
	flags: GroupFlags;
	trackAlias: bigint;
	groupId: number;
	subGroupId: number;
	publisherPriority: number;

	constructor(trackAlias: bigint, groupId: number, subGroupId: number, publisherPriority: number, flags: GroupFlags) {
		this.flags = flags;
		this.trackAlias = trackAlias;
		this.groupId = groupId;
		this.subGroupId = subGroupId;
		this.publisherPriority = publisherPriority;
	}

	async encode(w: Writer): Promise<void> {
		if (!this.flags.hasSubgroup && this.subGroupId !== 0) {
			throw new Error(`Subgroup ID must be 0 if hasSubgroup is false: ${this.subGroupId}`);
		}

		const base = this.flags.hasPriority ? 0x10 : 0x30;
		let id = base;
		if (this.flags.hasExtensions) {
			id |= 0x01;
		}
		if (this.flags.hasSubgroupObject) {
			id |= 0x02;
		}
		if (this.flags.hasSubgroup) {
			id |= 0x04;
		}
		if (this.flags.hasEnd) {
			id |= 0x08;
		}
		await w.u53(id);
		await w.u62(this.trackAlias);
		await w.u53(this.groupId);
		if (this.flags.hasSubgroup) {
			await w.u53(this.subGroupId);
		}
		if (this.flags.hasPriority) {
			await w.u8(this.publisherPriority);
		}
	}

	static async decode(r: Reader): Promise<Group> {
		const id = await r.u53();

		let hasPriority: boolean;
		let baseId: number;
		if (id >= 0x10 && id <= 0x1f) {
			hasPriority = true;
			baseId = id;
		} else if (id >= 0x30 && id <= 0x3f) {
			hasPriority = false;
			baseId = id - (0x30 - 0x10);
		} else {
			throw new Error(`Unsupported group type: ${id}`);
		}

		const flags: GroupFlags = {
			hasExtensions: (baseId & 0x01) !== 0,
			hasSubgroupObject: (baseId & 0x02) !== 0,
			hasSubgroup: (baseId & 0x04) !== 0,
			hasEnd: (baseId & 0x08) !== 0,
			hasPriority,
		};

		const trackAlias = await r.u62();
		const groupId = await r.u53();
		const subGroupId = flags.hasSubgroup ? await r.u53() : 0;
		const publisherPriority = hasPriority ? await r.u8() : 128; // Default priority when absent

		return new Group(trackAlias, groupId, subGroupId, publisherPriority, flags);
	}
}

export class Frame {
	// undefined means end of group
	payload?: Uint8Array;

	constructor(payload?: Uint8Array) {
		this.payload = payload;
	}

	async encode(w: Writer, flags: GroupFlags): Promise<void> {
		await w.u53(0); // id_delta = 0

		if (flags.hasExtensions) {
			await w.u53(0); // extensions length = 0
		}

		if (this.payload !== undefined) {
			await w.u53(this.payload.byteLength);

			if (this.payload.byteLength === 0) {
				await w.u53(0); // status = normal
			} else {
				await w.write(this.payload);
			}
		} else {
			await w.u53(0); // length = 0
			await w.u53(GROUP_END);
		}
	}

	static async decode(r: Reader, flags: GroupFlags): Promise<Frame> {
		const delta = await r.u53();
		if (delta !== 0) {
			console.warn(`object ID delta is not supported, ignoring: ${delta}`);
		}

		if (flags.hasExtensions) {
			const extensionsLength = await r.u53();
			// We don't care about extensions
			await r.read(extensionsLength);
		}

		const payloadLength = await r.u53();

		if (payloadLength > 0) {
			const payload = await r.read(payloadLength);
			return new Frame(payload);
		}

		const status = await r.u53();

		if (flags.hasEnd) {
			// Empty frame
			if (status === 0) return new Frame(new Uint8Array(0));
		} else if (status === 0 || status === GROUP_END) {
			// TODO status === 0 should be an empty frame, but moq-rs seems to be sending it incorrectly on group end.
			return new Frame();
		}

		throw new Error(`Unsupported object status: ${status}`);
	}
}
