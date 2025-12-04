import HangStats from "./element";

export default HangStats;

customElements.define("hang-stats", HangStats);

declare global {
    interface HTMLElementTagNameMap {
        "hang-stats": HangStats;
    }
}