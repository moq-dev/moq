import { resolve } from "path";
import { defineConfig } from "vite";
import { crossOriginIsolation } from "../../js/common/vite-plugin-isolate";
import { worklet } from "../../js/common/vite-plugin-worklet";

export default defineConfig({
        root: "src",
        envDir: resolve(__dirname),
        plugins: [worklet(), crossOriginIsolation()],
        build: { target: "esnext" },
        server: { hmr: false },
});

