/**
 * MoQ Boy: a Game Boy streaming viewer built on `@moq/watch`.
 *
 * The `<moq-boy>` element lives at `@moq/boy/element`; this entry holds its type and the status
 * a game publishes.
 *
 * @module
 */

export type { default as MoqBoy } from "./element.tsx";
export type { GameStats, GameStatus, LatencyEntry } from "./schemas.ts";
export { GameStatsSchema, GameStatusSchema } from "./schemas.ts";
