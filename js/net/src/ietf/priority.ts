/** Convert an IETF subscriber priority into the model's higher-first priority. */
export function fromWire(priority: number): number {
	return 0xff - priority;
}

/** Convert the model's higher-first priority into an IETF subscriber priority. */
export function toWire(priority: number): number {
	return 0xff - priority;
}
