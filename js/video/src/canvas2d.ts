import { canvasPresentationTransform, type Presentation } from "./presentation";

/** Draws frames into a Canvas2D context. */
export class Canvas2D {
	#ctx: CanvasRenderingContext2D | OffscreenCanvasRenderingContext2D;

	constructor(ctx: CanvasRenderingContext2D | OffscreenCanvasRenderingContext2D) {
		this.#ctx = ctx;
	}

	/** Draw `frame`, or clear to black without one. */
	draw(frame: VideoFrame | undefined, presentation: Presentation | undefined) {
		const ctx = this.#ctx;
		const { width, height } = ctx.canvas;

		ctx.save();
		ctx.fillStyle = "#000";
		ctx.fillRect(0, 0, width, height);

		if (frame) {
			const transform = canvasPresentationTransform({ width, height }, presentation);
			ctx.setTransform(...transform.matrix);
			const { width: w, height: h } = transform.source;
			ctx.drawImage(frame, 0, 0, w, h);

			if (presentation?.interlaced) {
				// The [1 2 1] line blend the WebGPU shader applies, built by compositing: a third of the
				// line above gives (2c + a) / 3, then a quarter of the line below gives (a + 2c + b) / 4.
				const line = h / (frame.visibleRect?.height ?? frame.codedHeight);
				ctx.globalAlpha = 1 / 3;
				ctx.drawImage(frame, 0, line, w, h);
				ctx.globalAlpha = 1 / 4;
				ctx.drawImage(frame, 0, -line, w, h);
			}
		}

		ctx.restore();
	}

	close() {}
}
