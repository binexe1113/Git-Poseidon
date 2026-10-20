// Real Tauri + Git diff viewer validation (docs/16). Disposable fixture only.
// Usage: node scripts/diff-smoke.mjs <runtime-bench binary> <output-dir>
// Hyprland >= 0.55: the window floats in the hidden `special:poseidon`
// workspace; scroll events are dispatched explicitly because a hidden window
// has no frame callbacks. Latencies are IPC + DOM update, not paint.
import { spawn, spawnSync } from 'node:child_process';
import {
  mkdtempSync,
  mkdirSync,
  writeFileSync,
  readFileSync,
  readdirSync,
} from 'node:fs';
import { join, relative } from 'node:path';
import { tmpdir } from 'node:os';
import { createHash } from 'node:crypto';
import assert from 'node:assert/strict';
import { connectInspector } from './webkit-inspector.mjs';

const [binary, outDir] = process.argv.slice(2);
if (!binary || !outDir)
  throw new Error('Usage: diff-smoke.mjs <binary> <output-dir>');
mkdirSync(outDir, { recursive: true });
const pause = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const root = mkdtempSync(join(tmpdir(), 'poseidon-diff-'));
const repo = join(root, 'diff-demo');
mkdirSync(repo);
const env = Object.fromEntries(
  Object.entries(process.env).filter(([key]) => !key.startsWith('GIT_')),
);
Object.assign(env, {
  HOME: root,
  XDG_CONFIG_HOME: join(root, 'xdg'),
  GIT_CONFIG_NOSYSTEM: '1',
  GIT_CONFIG_GLOBAL: join(root, 'no-global'),
  GIT_AUTHOR_NAME: 'Equipe Demo',
  GIT_AUTHOR_EMAIL: 'demo@example.invalid',
  GIT_COMMITTER_NAME: 'Equipe Demo',
  GIT_COMMITTER_EMAIL: 'demo@example.invalid',
  TZ: 'UTC',
});
let tick = 0;
function git(...args) {
  const date = `2025-02-0${1 + Math.min(8, tick)}T12:00:00+00:00`;
  const r = spawnSync('git', args, {
    cwd: repo,
    env: { ...env, GIT_AUTHOR_DATE: date, GIT_COMMITTER_DATE: date },
    encoding: 'utf8',
    maxBuffer: 64 * 1024 * 1024,
  });
  assert.equal(r.status, 0, `${args.join(' ')}: ${r.stderr}`);
  return r.stdout;
}
const write = (path, data) => {
  mkdirSync(join(repo, path, '..'), { recursive: true });
  writeFileSync(join(repo, path), data);
};
function commit(message) {
  tick++;
  git('add', '--all');
  git('commit', '-q', '-m', message);
  return git('rev-parse', 'HEAD').trim();
}
function fingerprint(path = repo, files = {}) {
  for (const entry of readdirSync(path, { withFileTypes: true })) {
    const next = join(path, entry.name);
    if (entry.isDirectory()) fingerprint(next, files);
    else
      files[relative(repo, next)] = createHash('sha256')
        .update(readFileSync(next))
        .digest('hex');
  }
  return files;
}

// --- Fixture -------------------------------------------------------------
git('init', '-q', '--initial-branch=main');
const rust = (variant) =>
  [
    '// Exemplo de serviço para o diff.',
    'use std::collections::HashMap;',
    '',
    ...Array.from({ length: 70 }, (_, i) =>
      i === 12 && variant
        ? `fn passo_${i}(valor: u32) -> u32 { valor * ${i} + 1 } // revisado`
        : i === 50 && variant
          ? `fn passo_${i}(valor: u32) -> u32 { valor.saturating_mul(${i}) }`
          : `fn passo_${i}(valor: u32) -> u32 { valor * ${i} }`,
    ),
    variant
      ? 'pub const VERSAO: &str = "0.2";'
      : 'pub const VERSAO: &str = "0.1";',
    '',
  ].join('\n');
write('src/main.rs', rust(false));
const readme = (extra) =>
  [
    '# Demo',
    '',
    'Projeto de demonstração.',
    ...Array.from({ length: 20 }, (_, i) => `Seção ${i}: conteúdo estável.`),
    extra,
    '',
  ].join('\n');
write('README.md', readme('Linha final.'));
write('docs/日本語 ✓ notas.md', 'primeira versão\n');
write('assets/logo.bin', Buffer.from([0, 1, 2, 3, 0, 255, 10, 0]));
write('obsoleto.txt', 'será removido\n');
const root0 = commit('Estrutura inicial');
write('src/main.rs', rust(true));
git('mv', 'README.md', 'docs/README.md');
write('docs/README.md', readme('Linha final revisada.'));
write('docs/日本語 ✓ notas.md', 'primeira versão\nsegunda linha\n');
write('assets/logo.bin', Buffer.from([0, 1, 2, 3, 0, 254, 10, 0, 9]));
git('rm', '-q', 'obsoleto.txt');
write('config/janelas.ini', 'largura=1280\r\naltura=720\r\n');
write(
  'data/tabela.csv',
  Array.from(
    { length: 12_000 },
    (_, i) => `${i},item-${i},${(i * 7) % 101}`,
  ).join('\n') + '\n',
);
write('data/registro-grande.log', ('x'.repeat(99) + '\n').repeat(30_000));
const changes = commit('Revisar serviço, documentação e dados');
git('switch', '-q', '-c', 'feature/tema');
write('src/tema.rs', 'pub const COR: &str = "#65d7bd";\n');
commit('Adicionar tema');
git('switch', '-q', 'main');
write('docs/README.md', readme('Linha final revisada.\nNota.'));
commit('Ajustar nota');
git('merge', '-q', '--no-ff', '-m', 'Merge feature/tema', 'feature/tema');
const merge = git('rev-parse', 'HEAD').trim();
const mergeFirst = git('rev-parse', 'HEAD^1').trim();
for (let i = 0; i < 16; i++) {
  write(`notas/nota-${i}.txt`, `nota ${i}\n`);
  commit(`Nota ${i}`);
}
const before = fingerprint();
// Oracle with -z: raw path bytes, no core.quotePath escaping.
const nameStatus = (range) => {
  const tokens = git(
    'diff-tree',
    '-r',
    '-M',
    '-z',
    '--name-status',
    ...range,
  ).split('\0');
  const records = [];
  for (let i = 0; i < tokens.length - 1;) {
    const status = tokens[i++];
    const paths = /^[RC]/.test(status)
      ? [tokens[i++], tokens[i++]]
      : [tokens[i++]];
    records.push([status, ...paths]);
  }
  return records;
};
const oracleHunks = (range, paths) => {
  const text = git(
    'diff-tree',
    '-p',
    '-r',
    '-M',
    '--no-color',
    ...range,
    '--',
    ...paths,
  );
  const start = text.indexOf('\n@@ ');
  return start < 0 ? '' : text.slice(start + 1);
};

// --- App -------------------------------------------------------------------
const endpoint = 'http://127.0.0.1:9234';
const app = spawn(binary, [], {
  env: { ...process.env, WEBKIT_INSPECTOR_HTTP_SERVER: '127.0.0.1:9234' },
  stdio: 'ignore',
});
let inspector;
const hypr = (lua) =>
  assert.equal(
    spawnSync('hyprctl', ['dispatch', lua], { encoding: 'utf8' }).stdout.trim(),
    'ok',
  );
function memory() {
  const p = spawnSync(
    'python3',
    [
      'scripts/measure-linux-memory.py',
      String(app.pid),
      '--samples',
      '8',
      '--interval',
      '0.25',
    ],
    { encoding: 'utf8' },
  );
  assert.equal(p.status, 0, p.stderr);
  const data = JSON.parse(p.stdout);
  return { rss_mib: data.rss_mib.p50, pss_mib: data.pss_mib.p50 };
}
const stats = (values) => {
  const sorted = [...values].sort((a, b) => a - b);
  return {
    samples: sorted.length,
    p50: sorted[Math.ceil(sorted.length * 0.5) - 1],
    p95: sorted[Math.ceil(sorted.length * 0.95) - 1],
    max: sorted.at(-1),
  };
};
const report = { checks: {}, latency_ms: {}, memory_mib: {}, dom: {} };
try {
  for (let i = 0; i < 100; i++) {
    const clients = JSON.parse(
      spawnSync('hyprctl', ['clients', '-j'], { encoding: 'utf8' }).stdout,
    );
    if (clients.some((c) => c.pid === app.pid)) break;
    await pause(100);
  }
  const window = `pid:${app.pid}`;
  hypr(
    `hl.dsp.window.move({ workspace = "special:poseidon", window = "${window}" })`,
  );
  hypr(`hl.dsp.window.float({ action = "enable", window = "${window}" })`);
  for (let i = 0; i < 100 && !inspector; i++) {
    try {
      inspector = await connectInspector(endpoint);
    } catch {
      await pause(100);
    }
  }
  await inspector.send('Runtime.enable');
  const ev = inspector.evaluate;
  const waitFor = async (expression, label = expression) => {
    for (let i = 0; i < 300; i++) {
      if (await ev(expression)) return;
      await pause(25);
    }
    throw new Error('UI condition failed: ' + label);
  };
  // WebKit's Runtime.evaluate does not await promises (awaitPromise is a
  // Chrome-only parameter): the page stores the settled value and we poll it.
  const evAsync = async (expression) => {
    await ev(
      `window.__r = undefined; Promise.resolve().then(() => ${expression}).then((v) => { window.__r = { v }; }, (e) => { window.__r = { e: String(e) }; }); true`,
    );
    await waitFor('window.__r !== undefined', expression.slice(0, 80));
    const result = await ev('window.__r');
    if ('e' in result) throw new Error(result.e);
    return result.v;
  };
  const resize = async (w, h) => {
    hypr(`hl.dsp.window.resize({ x = ${w}, y = ${h}, window = "${window}" })`);
    await waitFor(`innerWidth === ${w} && innerHeight === ${h}`);
    await pause(250);
  };
  const shot = async (name) => {
    await pause(350); // let 150 ms background transitions settle
    const s = await inspector.send('Page.snapshotRect', {
      x: 0,
      y: 0,
      width: await ev('innerWidth'),
      height: await ev('innerHeight'),
      coordinateSystem: 'Viewport',
    });
    writeFileSync(
      join(outDir, `${name}.png`),
      Buffer.from(s.dataURL.split(',')[1], 'base64'),
    );
  };
  // In-page helpers: timed click until a DOM condition holds (MutationObserver).
  await resize(1440, 900);
  await waitFor(
    '!!document.querySelector(".primary-button") && !document.querySelector(".primary-button").disabled',
  );
  await ev(`window.__until = (test, timeout = 15000) => new Promise((resolve, reject) => {
      if (test()) return resolve(0);
      const t0 = performance.now();
      const observer = new MutationObserver(() => { if (test()) { observer.disconnect(); resolve(performance.now() - t0); } });
      observer.observe(document.body, { subtree: true, childList: true, characterData: true, attributes: true });
      setTimeout(() => { observer.disconnect(); reject(new Error('timeout')); }, timeout);
    });
    window.__timed = async (action, test) => { const t0 = performance.now(); action(); await window.__until(test); return performance.now() - t0; };
    window.__row = (summary) => [...document.querySelectorAll('.commit-row')].find((r) => r.querySelector('.commit-subject strong')?.textContent === summary);
    window.__scroll = (el, top) => { el.scrollTop = top; el.dispatchEvent(new Event('scroll')); };
    true`);
  report.memory_mib.empty_shell = memory();
  await ev(
    `window.__o=window.fetch;window.fetch=(u,o)=>decodeURIComponent(String(u)).includes('plugin:dialog|open')?Promise.resolve(new Response(${JSON.stringify(JSON.stringify(repo))},{headers:{'Tauri-Response':'ok','Content-Type':'application/json'}})):window.__o(u,o);document.querySelector('.primary-button').click();true`,
  );
  await waitFor(
    'document.querySelectorAll(".commit-row").length > 10 && !!document.querySelector(".commit-message")',
  );
  await pause(300);
  report.memory_mib.repository_open = memory();

  // 0. Memory, isolated: fixed 1440x900, no resize or capture in between.
  const click = (selector, text) =>
    `[...document.querySelectorAll(${JSON.stringify(selector)})].find((e) => e.textContent.includes(${JSON.stringify(text)}) || e.title?.endsWith(${JSON.stringify(text)})).click()`;
  const traverse = async () => {
    const total = await ev(
      `+document.querySelector('.diff-body').dataset.totalLines`,
    );
    for (let step = 1; step <= 12; step++) {
      const target = Math.min(total - 1, Math.floor((total * step) / 12));
      await evAsync(
        `window.__timed(() => window.__scroll(document.querySelector('.diff-body'), ${target} * 20), () => !!document.querySelector('.diff-row[data-index="${target}"]'))`,
      );
    }
  };
  report.latency_ms.cold_file_list = await evAsync(
    `window.__timed(() => window.__row('Revisar serviço, documentação e dados').click(), () => document.querySelectorAll('.file-item').length === 8 && document.querySelector('.commit-files').dataset.oid === window.__row('Revisar serviço, documentação e dados').id.slice(7))`,
  );
  report.latency_ms.cold_small_diff_open = await evAsync(
    `window.__timed(() => ${click('.file-item', 'src/main.rs')}, () => !!document.querySelector('.diff-row'))`,
  );
  await pause(500);
  report.memory_mib.small_diff_open = memory();
  report.latency_ms.cold_12k_diff_open = await evAsync(
    `window.__timed(() => ${click('.file-item', 'data/tabela.csv')}, () => document.querySelector('.diff-body')?.dataset.totalLines === '12001')`,
  );
  await traverse();
  await pause(500);
  report.memory_mib.after_12k_lines_traversal = memory();
  await evAsync(
    `window.__timed(() => ${click('.file-item', 'data/registro-grande.log')}, () => !!document.querySelector('.diff-state .text-button'))`,
  );
  report.latency_ms.cold_3mib_30k_after_confirm = await evAsync(
    `window.__timed(() => ${click('.diff-state .text-button', 'Abrir mesmo assim')}, () => !!document.querySelector('.diff-row'))`,
  );
  await traverse();
  await pause(500);
  report.memory_mib.after_30k_lines_3mib_traversal = memory();
  await ev(
    `document.querySelector('.diff-viewer').dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true })); true`,
  );
  await waitFor('!document.querySelector(".diff-viewer")');
  await pause(3000);
  report.memory_mib.closed_after_3s = memory();
  await inspector.send('Heap.gc');
  await pause(1000);
  report.memory_mib.closed_after_diagnostic_gc = memory();

  // 1. File list of a commit with every change kind, against the Git oracle.
  const files = (summary) =>
    `window.__timed(() => window.__row(${JSON.stringify(summary)}).click(), () => { const row = window.__row(${JSON.stringify(summary)}); return !!row && document.querySelector('.commit-files')?.dataset.oid === row.id.slice(7) && document.querySelectorAll('.file-item').length > 0; })`;
  report.latency_ms.warm_file_list = await evAsync(
    files('Revisar serviço, documentação e dados'),
  );
  const listed = JSON.parse(
    await ev(
      `JSON.stringify([...document.querySelectorAll('.file-item')].map((b) => ({ badge: b.querySelector('.change-badge [aria-hidden]').textContent, path: b.title, stats: b.querySelector('.file-stats')?.textContent ?? null, binary: !!b.querySelector('.file-binary') })))`,
    ),
  );
  const oracle = nameStatus([root0, changes]);
  assert.equal(listed.length, oracle.length, 'file count');
  for (const [i, fields] of oracle.entries()) {
    assert.equal(listed[i].badge, fields[0][0], `status of ${fields.at(-1)}`);
    assert.ok(
      listed[i].path.endsWith(fields.at(-1)),
      `path ${fields.at(-1)} vs ${listed[i].path}`,
    );
  }
  assert.ok(listed.find((f) => f.path.endsWith('logo.bin')).binary);
  report.checks.file_list_matches_git = `${listed.length} arquivos, status e caminhos`;
  await shot('diff-files-1440x900');

  // 2. Small diff: latency and full content equality with git diff-tree -p.
  const openFile = (suffix) =>
    `window.__timed(() => [...document.querySelectorAll('.file-item')].find((b) => b.title.endsWith(${JSON.stringify(suffix)})).click(), () => document.querySelector('.diff-viewer h2')?.title.endsWith(${JSON.stringify(suffix)}) && (!!document.querySelector('.diff-row') || !!document.querySelector('.diff-state:not([role=status])')))`;
  const small = [];
  for (let i = 0; i < 7; i++) {
    small.push(await evAsync(openFile('src/main.rs')));
    await ev(
      `document.querySelector('.diff-viewer').dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true })); true`,
    );
    await waitFor('!document.querySelector(".diff-viewer")');
  }
  report.latency_ms.warm_small_diff_open = stats(small);
  await evAsync(openFile('src/main.rs'));
  const rendered = await ev(
    `[...document.querySelectorAll('.diff-row')].sort((a,b)=>a.dataset.index-b.dataset.index).map((r) => r.classList.contains('kind-hunk') ? r.textContent : (r.querySelector('.sg').textContent.replace('−','-') + r.querySelector('.tx').textContent.replace(/␍$/, '\\r'))).join('\\n') + '\\n'`,
  );
  assert.equal(
    rendered,
    oracleHunks([root0, changes], ['src/main.rs']),
    'main.rs diff differs from git',
  );
  report.checks.small_diff_equals_git = true;
  report.checks.highlight_spans = await ev(
    `document.querySelectorAll('.diff-row .hl-k, .diff-row .hl-c, .diff-row .hl-s').length`,
  );
  await ev(
    `document.querySelector('.diff-viewer').dispatchEvent(new KeyboardEvent('keydown', { key: 'n', bubbles: true })); true`,
  );
  await pause(100);
  report.checks.hunk_navigation = await ev(
    `document.querySelector('.hunk-nav').textContent.replace(/\\s+/g,' ').trim()`,
  );
  assert.match(report.checks.hunk_navigation, /Hunk 2 de 3/);
  assert.equal(
    await ev(`document.querySelectorAll('.diff-row.kind-hunk.current').length`),
    1,
  );
  for (const [w, h] of [
    [1280, 720],
    [1366, 768],
    [1440, 900],
    [1920, 1080],
    [1000, 900],
  ]) {
    await resize(w, h);
    const geometry = JSON.parse(
      await ev(
        `JSON.stringify((() => { const b = document.querySelector('.diff-body').getBoundingClientRect(); const v = document.querySelector('.diff-viewer').getBoundingClientRect(); return { viewer: [Math.round(v.width), Math.round(v.height)], body_height: Math.round(b.height), overflowX: document.documentElement.scrollWidth > innerWidth, inspectorVisible: !!document.querySelector('#inspector:not([hidden])') }; })())`,
      ),
    );
    report.dom[`diff_${w}x${h}`] = geometry;
    assert.equal(geometry.overflowX, false);
    await shot(`diff-small-${w}x${h}`);
  }
  await resize(1440, 900);

  // 3. Rename, CRLF and Unicode paths, binary.
  await evAsync(openFile('docs/README.md'));
  report.checks.rename_label = await ev(
    `document.querySelector('.diff-meta').textContent.includes('renomeado de README.md')`,
  );
  assert.ok(report.checks.rename_label);
  await evAsync(openFile('janelas.ini'));
  report.checks.crlf_markers = await ev(
    `document.querySelectorAll('.diff-row .eol').length`,
  );
  assert.ok(report.checks.crlf_markers >= 2);
  await evAsync(openFile('日本語 ✓ notas.md'));
  assert.ok(
    await ev(
      `[...document.querySelectorAll('.diff-row.kind-added .tx')].some((t) => t.textContent === 'segunda linha')`,
    ),
  );
  report.checks.unicode_path_diff = true;
  await evAsync(openFile('assets/logo.bin'));
  assert.ok(
    await ev(
      `document.querySelector('.diff-state')?.textContent.includes('Arquivo binário')`,
    ),
  );
  report.checks.binary_state = await ev(
    `document.querySelector('.diff-state').textContent.replace(/\\s+/g,' ').trim()`,
  );
  await shot('diff-binary-1440x900');

  // 4. Thousands of lines: paging, virtualization and memory.
  report.latency_ms.warm_12k_open = await evAsync(openFile('data/tabela.csv'));
  const total = await ev(
    `+document.querySelector('.diff-body').dataset.totalLines`,
  );
  assert.equal(total, 12_001);
  report.dom.table_rows_at_top = await ev(
    `document.querySelectorAll('.diff-row').length`,
  );
  const visible = await ev(
    `Math.ceil(document.querySelector('.diff-body').clientHeight / 20)`,
  );
  assert.ok(
    report.dom.table_rows_at_top <= visible + 2 * 40 + 1,
    `DOM ${report.dom.table_rows_at_top}`,
  );
  report.latency_ms.table_jump_to_end = await evAsync(
    `window.__timed(() => { const b = document.querySelector('.diff-body'); window.__scroll(b, b.scrollHeight); }, () => !!document.querySelector('.diff-row[data-index="12000"]'))`,
  );
  const lastLine = await ev(
    `document.querySelector('.diff-row[data-index="12000"] .tx').textContent`,
  );
  assert.equal(lastLine, '11999,item-11999,' + ((11999 * 7) % 101));
  const middle = [];
  for (const target of [3000, 7000, 5000, 9500, 1000]) {
    middle.push(
      await evAsync(
        `window.__timed(() => window.__scroll(document.querySelector('.diff-body'), ${target} * 20), () => !!document.querySelector('.diff-row[data-index="${target + 5}"]'))`,
      ),
    );
    assert.equal(
      await ev(
        `document.querySelector('.diff-row[data-index="${target + 5}"] .tx').textContent`,
      ),
      `${target + 4},item-${target + 4},${((target + 4) * 7) % 101}`,
    );
  }
  report.latency_ms.table_random_scroll = stats(middle);
  report.dom.table_rows_max = await ev(
    `document.querySelectorAll('.diff-row').length`,
  );
  await shot('diff-12k-lines-1440x900');

  // 5. Soft limit gate (3 MiB), then explicit open.
  await evAsync(openFile('data/registro-grande.log'));
  assert.ok(
    await ev(
      `document.querySelector('.diff-state')?.textContent.includes('Arquivo grande')`,
    ),
  );
  await shot('diff-large-gate-1440x900');
  report.latency_ms.warm_3mib_after_confirm = await evAsync(
    `window.__timed(() => [...document.querySelectorAll('.diff-state .text-button')].find((b) => b.textContent.includes('Abrir mesmo assim')).click(), () => !!document.querySelector('.diff-row'))`,
  );
  report.checks.large_total_lines = await ev(
    `+document.querySelector('.diff-body').dataset.totalLines`,
  );
  report.checks.large_truncated_banner = await ev(
    `!!document.querySelector('.diff-banner')`,
  );
  await ev(
    `document.querySelector('.diff-viewer').dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true })); true`,
  );
  await waitFor('!document.querySelector(".diff-viewer")');
  report.checks.focus_returns_to_file = await ev(
    `document.activeElement?.classList.contains('file-item')`,
  );

  // 6. Merge: first parent, labelled.
  await evAsync(files('Merge feature/tema'));
  const base = await ev(
    `document.querySelector('.diff-base').textContent.replace(/\\s+/g,' ').trim()`,
  );
  assert.ok(
    base.includes('Merge') && base.includes(mergeFirst.slice(0, 8)),
    base,
  );
  const mergeFiles = nameStatus([mergeFirst, merge]).map((f) => f.at(-1));
  assert.deepEqual(
    await ev(
      `[...document.querySelectorAll('.file-item')].map((b) => b.title)`,
    ),
    mergeFiles,
  );
  report.checks.merge_first_parent = base;
  await shot('diff-merge-files-1440x900');

  // 7. Rapid selection: 16 clicks without waiting; the last one must win.
  const rapid = await evAsync(`(async () => {
      const t0 = performance.now();
      for (let i = 15; i >= 0; i--) window.__row('Nota ' + i).click();
      await window.__until(() => document.querySelector('.file-item')?.title === 'notas/nota-0.txt' && document.querySelectorAll('.file-item').length === 1);
      return { ms: performance.now() - t0, errors: document.querySelectorAll('.detail-error, .error-banner').length, selected: document.querySelector('.commit-row.selected .commit-subject strong').textContent };
    })()`);
  assert.equal(rapid.errors, 0);
  assert.equal(rapid.selected, 'Nota 0');
  report.latency_ms.rapid_16_selections_settled = rapid.ms;
  await pause(500);
  report.checks.rapid_final_state_stable = await ev(
    `document.querySelector('.file-item')?.title === 'notas/nota-0.txt' && document.querySelectorAll('.file-item').length === 1`,
  );
  assert.ok(report.checks.rapid_final_state_stable);

  // 8. The graph is intact after the overlay: row step and SVG alignment.
  const graph = JSON.parse(
    await ev(
      `JSON.stringify((() => { const rows = [...document.querySelectorAll('.commit-row')].sort((a,b)=>a.dataset.index-b.dataset.index); return { step: rows[1].getBoundingClientRect().top - rows[0].getBoundingClientRect().top, drift: Math.max(...rows.map((r) => Math.abs(r.getBoundingClientRect().top - r.querySelector('svg').getBoundingClientRect().top))), inert: document.querySelector('.history-host').inert }; })())`,
    ),
  );
  assert.equal(Math.round(graph.step), 30);
  assert.ok(graph.drift < 1);
  assert.equal(graph.inert, false);
  report.checks.graph_after_diff = graph;
  report.memory_mib.final = memory();
  assert.deepEqual(
    fingerprint(),
    before,
    'diff inspection changed the fixture',
  );
  report.checks.fixture_unchanged = true;
  report.pass = true;
  writeFileSync(
    join(outDir, 'diff-smoke.json'),
    JSON.stringify(report, null, 2) + '\n',
  );
  console.log(JSON.stringify(report, null, 2));
} finally {
  inspector?.close();
  app.kill('SIGTERM');
}
