import { readFileSync, writeFileSync } from "fs";
import { Resvg } from "@resvg/resvg-js";

const WIDTH = 1200;
const HEIGHT = 630;

// Read the icon SVG and extract just the paths
const iconSvg = readFileSync("doc/public/icon.svg", "utf-8");

// Extract the path elements and style from icon.svg
const paths = iconSvg.match(/<path[^>]*\/>/g)?.join("\n") || "";
const style = iconSvg.match(/<style>[\s\S]*?<\/style>/)?.[0] || "";

// Create an OG-optimized SVG with the icon centered and tagline
const ogSvg = `<?xml version="1.0" encoding="utf-8"?>
<svg viewBox="0 0 ${WIDTH} ${HEIGHT}" xmlns="http://www.w3.org/2000/svg" width="${WIDTH}" height="${HEIGHT}">
  <rect width="${WIDTH}" height="${HEIGHT}" fill="#0f172a"/>

  <!-- Icon centered, scaled to fit nicely -->
  <g transform="translate(${WIDTH / 2 - 175}, ${HEIGHT / 2 - 220}) scale(0.7)">
    <rect x="-25" y="-25" width="550" height="550" fill="#0f172a" rx="50" ry="50" />
    ${paths}
  </g>

  <!-- Tagline -->
  <text x="${WIDTH / 2}" y="${HEIGHT - 100}" text-anchor="middle" font-family="system-ui, -apple-system, sans-serif" font-size="36" fill="#94a3b8" font-weight="400">
    Real-time latency at massive scale
  </text>

  ${style}
</svg>`;

const resvg = new Resvg(ogSvg, {
	fitTo: { mode: "width" as const, value: WIDTH },
	font: {
		loadSystemFonts: true,
	},
});

const pngData = resvg.render();
const pngBuffer = pngData.asPng();

writeFileSync("doc/public/og-image.png", pngBuffer);
console.log(`Generated og-image.png (${pngBuffer.length} bytes)`);
