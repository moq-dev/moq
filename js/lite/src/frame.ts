import * as Time from "./time";

export class Frame {
	timestamp: Time.Micro;
	payload: Uint8Array;

	constructor({ payload, timestamp = Time.Micro.now() }: { payload: Uint8Array; timestamp?: Time.Micro }) {
		this.timestamp = timestamp;
		this.payload = payload;
	}

	static fromString(str: string, timestamp = Time.Micro.now()) {
		return new Frame({ payload: new TextEncoder().encode(str), timestamp });
	}

	static fromJson(json: unknown, timestamp = Time.Micro.now()) {
		return new Frame({ payload: new TextEncoder().encode(JSON.stringify(json)), timestamp });
	}

	static fromBool(bool: boolean, timestamp = Time.Micro.now()) {
		return new Frame({ payload: new Uint8Array([bool ? 1 : 0]), timestamp });
	}

	toString() {
		return new TextDecoder().decode(this.payload);
	}

	toJson() {
		return JSON.parse(this.toString());
	}

	toBool() {
		if (this.payload.byteLength !== 1) throw new Error("invalid bool frame");
		if (this.payload[0] === 0) return false;
		if (this.payload[0] === 1) return true;
		throw new Error("invalid bool frame");
	}
}
