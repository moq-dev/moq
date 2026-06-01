import { defineConfig } from "vite";
import { workletInline } from "../../js/common/vite-plugin-worklet";

// workletInline handles the `?worklet` imports in @moq/publish; esnext keeps
// WebCodecs/WebTransport syntax intact.
export default defineConfig({
	plugins: [workletInline()],
	build: { target: "esnext" },
});
