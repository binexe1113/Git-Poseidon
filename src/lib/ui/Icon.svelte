<script lang="ts">
  let { name, size = 18 }: { name: string; size?: number } = $props();
  const paths: Record<string, string> = {
    folder:
      'M3 7a2 2 0 0 1 2-2h5l2 2h7a2 2 0 0 1 2 2v10a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2Z',
    branch:
      'M6 3v12a3 3 0 1 0 3 3c0-5 9-3 9-9V6 M3 3a3 3 0 1 0 6 0a3 3 0 1 0-6 0 M15 3a3 3 0 1 0 6 0a3 3 0 1 0-6 0',
    history: 'M3 11a9 9 0 1 1 2.4 7 M3 4v7h7 M12 7v5l3 2',
    refresh:
      'M20 7v5h-5 M4 17v-5h5 M5.5 7a8 8 0 0 1 13-2L20 7 M4 17l1.5 2a8 8 0 0 0 13-2',
    commit: 'M3 12h5m8 0h5 M8 12a4 4 0 1 0 8 0a4 4 0 1 0-8 0',
    chevron: 'm9 5 7 7-7 7',
    down: 'm6 9 6 6 6-6',
    up: 'm6 15 6-6 6 6',
    file: 'M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8Z M14 2v6h6 M8 13h8 M8 17h5',
    search: 'M10.5 3a7.5 7.5 0 1 0 0 15a7.5 7.5 0 1 0 0-15 M16 16l5 5',
    shield: 'M12 3 3 7v5c0 5 9 9 9 9s9-4 9-9V7Z m-4 9 3 3 5-6',
    terminal: 'm4 6 6 6-6 6 M13 18h7',
    cloud: 'M6 18a5 5 0 0 1-1-10a7 7 0 0 1 13-1a5.5 5.5 0 0 1 0 11Z',
    alert: 'M12 3 2 21h20Z M12 9v5 M12 17v1',
    check: 'm5 12 4 4L19 6',
    clock: 'M12 3a9 9 0 1 0 0 18a9 9 0 1 0 0-18 M12 7v5l3 2',
    close: 'm6 6 12 12 M6 18 18 6',
    arrow: 'M5 12h14 m-5-5 5 5-5 5',
    tag: 'M3 12V4a1 1 0 0 1 1-1h8l9 9-9 9Z M8 8v.01',
    target:
      'M12 3a9 9 0 1 0 0 18a9 9 0 1 0 0-18 M12 9a3 3 0 1 0 0 6a3 3 0 1 0 0-6',
    panel: 'M4 4h16v16H4Z M15 4v16',
    plus: 'M12 5v14 M5 12h14',
    minus: 'M5 12h14',
    unlock: 'M5 11h14v10H5Z M8 11V7a4 4 0 0 1 7.5-2',
  };
</script>

<svg
  width={size}
  height={size}
  viewBox="0 0 24 24"
  fill="none"
  stroke="currentColor"
  stroke-width="1.6"
  stroke-linecap="round"
  stroke-linejoin="round"
  aria-hidden="true"
>
  <path d={paths[name] ?? paths.file} />
</svg>
