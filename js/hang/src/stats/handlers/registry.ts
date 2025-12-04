import type { HandlerConstructor, Icons } from "../types";
import { VideoHandler } from "./video";
import { AudioHandler } from "./audio";
import { BufferHandler } from "./buffer";
import { NetworkHandler } from "./network";

/**
 * Registry mapping metric types to their handler implementations
 */
export const handlerRegistry: Record<Icons, HandlerConstructor> = {
	video: VideoHandler,
	audio: AudioHandler,
	buffer: BufferHandler,
	network: NetworkHandler,
};

/**
 * Get handler class for a metric type
 * @param icon - Metric type identifier
 * @returns Handler constructor or undefined if not found
 */
export function getHandlerClass(icon: Icons): HandlerConstructor | undefined {
	return handlerRegistry[icon];
}
