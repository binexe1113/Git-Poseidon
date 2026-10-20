// One row geometry shared by virtualization, SVG and CSS (`--row-height`).
// Rust emits lanes and half-row coordinates; only the pixel scale lives here.
export const ROW_HEIGHT = 30;
export const LANE_WIDTH = 16;
const GRAPH_INSET = 12;

export const laneX = (lane: number) => GRAPH_INSET + lane * LANE_WIDTH;
export const graphWidth = (lanes: number) =>
  Math.max(1, lanes) * LANE_WIDTH + GRAPH_INSET + 8;
/** Rust half-row coordinate (top=0, node=1, bottom=2) to pixels. */
export const halfRowY = (value: number) => (value * ROW_HEIGHT) / 2;
