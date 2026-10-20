<script lang="ts">
  import type { GraphRow, GraphSegment } from '../../lib/ipc/bindings';
  import { ROW_HEIGHT, graphWidth, halfRowY, laneX } from './geometry';
  let { row, merge }: { row: GraphRow; merge: boolean } = $props();
  const colors = [
    '#5ec7ee',
    '#f0b35d',
    '#b29aff',
    '#67d5b1',
    '#f78cb4',
    '#a4ce68',
    '#f18e69',
    '#89b4fa',
    '#d4bfff',
    '#ead486',
    '#6acac6',
    '#df9be6',
  ];
  const middle = ROW_HEIGHT / 2;
  function path(s: GraphSegment) {
    const a = laneX(s.fromLane),
      b = laneX(s.toLane),
      y = halfRowY(s.fromY),
      z = halfRowY(s.toY);
    return `M ${a} ${y} C ${a} ${(y + z) / 2} ${b} ${(y + z) / 2} ${b} ${z}`;
  }
</script>

<svg
  class="commit-graph"
  height={ROW_HEIGHT}
  width={graphWidth(row.width)}
  aria-hidden="true"
>
  {#each row.segments as segment}
    <path
      d={path(segment)}
      stroke={colors[segment.color % colors.length]}
      fill="none"
      stroke-width="2"
      data-child={segment.child}
      data-parent={segment.parent}
    />
  {/each}
  {#if merge}<circle
      class="merge-ring"
      cx={laneX(row.lane)}
      cy={middle}
      r="7"
      stroke={colors[row.lane % colors.length]}
      fill="#121b28"
      stroke-width="1.5"
    />{/if}
  <circle
    class="graph-node"
    cx={laneX(row.lane)}
    cy={middle}
    r={merge ? 3.6 : 4.4}
    fill={colors[row.lane % colors.length]}
    stroke="#e4f2fc"
    stroke-width="1.5"
  />
</svg>
