import type { SharedRingBufferInit } from "./shared-ring-buffer";

export type Message = Init;

export interface Init extends SharedRingBufferInit {
	type: "init";
}
