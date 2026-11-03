// The workspace imports src/ directly with no build step, so it reports "dev".
// `js/common/package.ts` stamps the package.json version into dist/version.js.
export const VERSION: string = "dev";
