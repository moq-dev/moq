/**
 * A reconnecting, shareable handle on a MoQ session, plus one-shot connect/accept.
 *
 * @module
 */
export { type AcceptProps, accept } from "./accept.ts";
export { type ConnectProps, connect } from "./connect.ts";
export { Connection } from "./pool.ts";
export type { Session } from "./session.ts";
