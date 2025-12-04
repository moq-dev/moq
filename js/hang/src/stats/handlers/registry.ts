import type { HandlerConstructor, Icons } from "../types";
import { VideoHandler } from "./video";
import { AudioHandler } from "./audio";
import { BufferHandler } from "./buffer";
import { NetworkHandler } from "./network";

export const handlerRegistry: Record<Icons, HandlerConstructor> = {
	video: VideoHandler,
	audio: AudioHandler,
	buffer: BufferHandler,
	network: NetworkHandler,
};

export function getHandlerClass(icon: Icons): HandlerConstructor | undefined {
	return handlerRegistry[icon];
}
