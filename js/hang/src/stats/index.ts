import HangStats from "./element";

/**
 * Export hang-stats web component as default
 */
export default HangStats;

/**
 * Register hang-stats custom element
 */
customElements.define("hang-stats", HangStats);

/**
 * TypeScript declaration for hang-stats element
 */
declare global {
    interface HTMLElementTagNameMap {
        "hang-stats": HangStats;
    }
}