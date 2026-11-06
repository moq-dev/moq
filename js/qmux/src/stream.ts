import { VarInt } from "./varint.ts";

export const Dir = {
	Bi: 0,
	Uni: 1,
} as const;

export type DirType = (typeof Dir)[keyof typeof Dir];

export class Id {
	readonly value: VarInt;

	constructor(value: VarInt) {
		this.value = value;
	}

	static create(id: bigint, dir: DirType, isServer: boolean): Id {
		let streamId = id << 2n;
		if (dir === Dir.Uni) {
			streamId |= 0x02n;
		}
		if (isServer) {
			streamId |= 0x01n;
		}
		return new Id(VarInt.from(streamId));
	}

	get dir(): DirType {
		return (this.value.value & 0x02n) !== 0n ? Dir.Uni : Dir.Bi;
	}

	get serverInitiated(): boolean {
		return (this.value.value & 0x01n) !== 0n;
	}

	/** Returns the 0-based sequence index of this stream. */
	get index(): bigint {
		return this.value.value >> 2n;
	}

	canRecv(isServer: boolean): boolean {
		if (this.dir === Dir.Uni) {
			return this.serverInitiated !== isServer;
		}
		return true;
	}

	canSend(isServer: boolean): boolean {
		if (this.dir === Dir.Uni) {
			return this.serverInitiated === isServer;
		}
		return true;
	}
}
