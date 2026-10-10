/**
 * Draw video frames into a canvas.
 *
 * {@link Renderer} paints `VideoFrame`s through WebGPU, importing each one as an external texture
 * in its own colour space, and through Canvas2D where WebGPU cannot. `@moq/watch` and the
 * `@moq/publish` preview both render through it.
 *
 * ```ts
 * import * as Video from "@moq/video";
 *
 * const canvas = new Signal(document.querySelector("canvas") ?? undefined);
 * const renderer = new Video.Renderer({ canvas, frame, presentation: { rotation: 90 } });
 *
 * // A canvas that lost its GPU never hands out another context, so swap in a fresh one.
 * renderer.out.error.subscribe((error) => {
 * 	const old = canvas.peek();
 * 	if (error !== "surface-lost" || !old) return;
 * 	const fresh = old.cloneNode() as HTMLCanvasElement;
 * 	old.replaceWith(fresh);
 * 	canvas.set(fresh);
 * });
 * ```
 *
 * @module
 */
export { type Dimensions, type Presentation, rotateVideoDimensions } from "./presentation";
export * from "./renderer";
