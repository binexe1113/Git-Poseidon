// Real Tauri validation of local diffs: HEAD → index, index → working tree and
// untracked files, plus invalidation after external changes (docs/16).
// Disposable fixture only. Usage: node scripts/working-smoke.mjs <runtime-bench binary> <output-dir>
// Hyprland >= 0.55: the window floats in the hidden `special:poseidon` workspace;
// scroll and focus events are dispatched explicitly (no frame callbacks there).
// Latencies are IPC + DOM update, not paint.
import { spawn, spawnSync } from 'node:child_process';
import {
  mkdtempSync,
  mkdirSync,
  writeFileSync,
  readFileSync,
  readdirSync,
  appendFileSync,
  rmSync,
} from 'node:fs';
import { join, relative } from 'node:path';
import { tmpdir } from 'node:os';
import { createHash } from 'node:crypto';
import assert from 'node:assert/strict';
import { connectInspector } from './webkit-inspector.mjs';

const [binary, outDir] = process.argv.slice(2);
if (!binary || !outDir)
  throw new Error('Usage: working-smoke.mjs <binary> <output-dir>');
mkdirSync(outDir, { recursive: true });
const pause = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const root = mkdtempSync(join(tmpdir(), 'poseidon-working-'));
const repo = join(root, 'working-demo');
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
  GIT_AUTHOR_DATE: '2025-03-01T12:00:00+00:00',
  GIT_COMMITTER_DATE: '2025-03-01T12:00:00+00:00',
  TZ: 'UTC',
});
function run(args, ok = [0]) {
  const r = spawnSync('git', args, {
    cwd: repo,
    env,
    encoding: 'utf8',
    maxBuffer: 64 * 1024 * 1024,
  });
  assert.ok(ok.includes(r.status), `${args.join(' ')}: ${r.stderr}`);
  return r.stdout;
}
const git = (...args) => run(args);
const write = (path, data) => {
  mkdirSync(join(repo, path, '..'), { recursive: true });
  writeFileSync(join(repo, path), data);
};
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
const code = (variant) =>
  [
    '// Serviço de exemplo.',
    ...Array.from({ length: 40 }, (_, i) =>
      variant >= 1 && i === 5
        ? `fn etapa_${i}() -> u32 { ${i} + 1 } // preparado`
        : variant >= 2 && i === 30
          ? `fn etapa_${i}() -> u32 { ${i} * 2 } // em edição`
          : `fn etapa_${i}() -> u32 { ${i} }`,
    ),
    '',
  ].join('\n');
const readme = [
  '# Demo',
  ...Array.from({ length: 20 }, (_, i) => `Seção ${i}: texto estável.`),
  '',
].join('\n');
write('src/app.rs', code(0));
write('README.md', readme);
write('config/janelas.ini', 'largura=1280\r\naltura=720\r\n');
write('obsoleto.txt', 'será removido\n');
write('rascunho.txt', 'apagado só no working tree\n');
git('add', '--all');
git('commit', '-q', '-m', 'Base');
const head = git('rev-parse', 'HEAD').trim();
// Staged: modify, add, rename, delete.
write('src/app.rs', code(1));
write('docs/novo.md', '# Novo\n\nconteúdo preparado\n');
git('mv', 'README.md', 'docs/LEIAME.md');
git('rm', '-q', 'obsoleto.txt');
git('add', 'src/app.rs', 'docs/novo.md');
// Unstaged: partial staging, CRLF, worktree deletion.
write('src/app.rs', code(2));
write('config/janelas.ini', 'largura=1366\r\naltura=768\r\n');
rmSync(join(repo, 'rascunho.txt'));
// Untracked: Unicode path, binary, 12k lines, above the 2 MiB gate.
write('notas/日本語 ✓ ideia.md', 'primeira ideia\nsegunda ideia\n');
write('assets/novo.bin', Buffer.from([0, 1, 2, 0, 255, 7]));
const tableLine = (i) => `${i};registro-${i};${(i * 13) % 97}`;
write(
  'dados/tabela.csv',
  Array.from({ length: 12_000 }, (_, i) => tableLine(i)).join('\n') + '\n',
);
write('dados/registro.log', ('y'.repeat(99) + '\n').repeat(30_000));
const before = fingerprint();

// Oracles: Git's own lists and patches (raw paths with -z).
const pairs = (text) => {
  const tokens = text.split('\0');
  const records = [];
  for (let i = 0; i < tokens.length - 1;) {
    const status = tokens[i++];
    const paths = /^[RC]/.test(status)
      ? [tokens[i++], tokens[i++]]
      : [tokens[i++]];
    records.push([status[0], paths.at(-1), paths.length > 1 ? paths[0] : null]);
  }
  return records;
};
const oracle = () => ({
  staged: pairs(git('diff', '--cached', '-M', '-z', '--name-status')),
  unstaged: pairs(git('diff', '-z', '--name-status')),
  untracked: git('ls-files', '-z', '--others', '--exclude-standard')
    .split('\0')
    .filter(Boolean)
    .map((path) => ['N', path, null]),
});
const hunks = (text) => {
  const start = text.indexOf('\n@@ ');
  return start < 0 ? '' : text.slice(start + 1);
};
const oraclePatch = (area, path) =>
  hunks(
    area === 'staged'
      ? git(
          '-c',
          'core.quotePath=false',
          'diff',
          '--cached',
          '-M',
          '--no-color',
          '--',
          path,
        )
      : area === 'unstaged'
        ? git('-c', 'core.quotePath=false', 'diff', '--no-color', '--', path)
        : run(
            ['diff', '--no-index', '--no-color', '--', '/dev/null', path],
            [1],
          ),
  );

// --- App -------------------------------------------------------------------
const endpoint = 'http://127.0.0.1:9235';
const app = spawn(binary, [], {
  env: { ...process.env, WEBKIT_INSPECTOR_HTTP_SERVER: '127.0.0.1:9235' },
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
  // WebKit's Runtime.evaluate does not await promises: poll a settled global.
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
    await pause(350);
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
    window.__scroll = (el, top) => { el.scrollTop = top; el.dispatchEvent(new Event('scroll')); };
    window.__item = (area, path) => [...document.querySelectorAll('#inspector-changes .working-group')].find((g) => g.querySelector('h4').id === 'group-' + area)?.querySelectorAll('.file-item') && [...[...document.querySelectorAll('#inspector-changes .working-group')].find((g) => g.querySelector('h4').id === 'group-' + area).querySelectorAll('.file-item')].find((b) => b.title.split(' · ')[0] === path || b.title.split(' · ')[0].endsWith(' → ' + path));
    window.__revision = () => document.querySelector('#inspector-changes .changes-panel')?.dataset.revision;
    window.__rendered = () => [...document.querySelectorAll('.diff-row')].sort((a,b)=>a.dataset.index-b.dataset.index).map((r) => r.classList.contains('kind-hunk') ? r.textContent : r.classList.contains('kind-note') ? '\\\\ ' + r.querySelector('.tx').textContent : (r.querySelector('.sg').textContent.replace('−','-') + r.querySelector('.tx').textContent.replace(/␍$/, '\\r'))).join('\\n') + '\\n';
    true`);
  await ev(
    `window.__o=window.fetch;window.fetch=(u,o)=>decodeURIComponent(String(u)).includes('plugin:dialog|open')?Promise.resolve(new Response(${JSON.stringify(JSON.stringify(repo))},{headers:{'Tauri-Response':'ok','Content-Type':'application/json'}})):window.__o(u,o);document.querySelector('.primary-button').click();true`,
  );
  await waitFor(
    'document.querySelectorAll(".commit-row").length >= 1 && !!document.querySelector(".commit-message")',
  );
  await pause(300);
  report.memory_mib.repository_open = memory();

  // 1. Listing against Git, per group.
  const expected = oracle();
  const total = Object.values(expected).reduce((n, l) => n + l.length, 0);
  report.latency_ms.cold_working_list = await evAsync(
    `window.__timed(() => document.querySelector('#tab-changes').click(), () => document.querySelectorAll('#inspector-changes .file-item').length === ${total})`,
  );
  const listed = JSON.parse(
    await ev(
      `JSON.stringify(Object.fromEntries([...document.querySelectorAll('#inspector-changes .working-group')].map((g) => [g.querySelector('h4').id.slice(6), [...g.querySelectorAll('.file-item')].map((b) => [b.querySelector('.change-badge [aria-hidden]').textContent, b.title.split(' · ')[0]])])))`,
    ),
  );
  for (const area of ['staged', 'unstaged', 'untracked']) {
    assert.deepEqual(
      listed[area],
      expected[area].map(([status, path, old]) => [
        status,
        old ? `${old} → ${path}` : path,
      ]),
      `${area} list differs from git`,
    );
  }
  report.checks.lists_match_git = Object.fromEntries(
    Object.entries(listed).map(([k, v]) => [k, v.length]),
  );
  report.checks.ranges = await ev(
    `[...document.querySelectorAll('#inspector-changes .working-group .diff-base')].map((p) => p.textContent.trim())`,
  );
  assert.equal(report.checks.ranges[0], `HEAD ${head.slice(0, 8)} → index`);

  // 2. Patches equal Git's, per group; header names the comparison.
  const open = (area, path) =>
    `window.__timed(() => window.__item(${JSON.stringify(area)}, ${JSON.stringify(path)}).click(), () => document.querySelector('.diff-viewer h2')?.title === ${JSON.stringify(path)} && document.querySelector('.diff-range')?.textContent.includes(${JSON.stringify(area === 'staged' ? '→ index' : '→ working tree')}) && (!!document.querySelector('.diff-row') || !!document.querySelector('.diff-state:not([role=status])')))`;
  const opened = {};
  for (const [area, path] of [
    ['staged', 'src/app.rs'],
    ['staged', 'docs/LEIAME.md'],
    ['staged', 'docs/novo.md'],
    ['unstaged', 'src/app.rs'],
    ['unstaged', 'config/janelas.ini'],
    ['unstaged', 'rascunho.txt'],
    ['untracked', 'notas/日本語 ✓ ideia.md'],
  ]) {
    opened[`${area}:${path}`] = await evAsync(open(area, path));
    if (path === 'docs/LEIAME.md') {
      assert.ok(
        await ev(
          `document.querySelector('.diff-meta').textContent.includes('renomeado de README.md')`,
        ),
      );
      continue; // pure rename: no hunks
    }
    assert.equal(
      await ev('window.__rendered()'),
      oraclePatch(area, path),
      `${area} ${path}`,
    );
    if (area === 'staged' && path === 'src/app.rs')
      await shot('working-staged-1440x900');
    if (area === 'untracked') await shot('working-untracked-unicode-1440x900');
  }
  report.latency_ms.open_small_local_diff = opened;
  report.checks.patches_equal_git = Object.keys(opened).length;
  report.checks.range_labels = {
    unstaged: 'index → working tree',
    staged: `HEAD ${head.slice(0, 8)} → index`,
  };
  await evAsync(open('unstaged', 'src/app.rs'));
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
        `JSON.stringify((() => { const v = document.querySelector('.diff-viewer').getBoundingClientRect(); const items = [...document.querySelectorAll('#inspector-changes .file-item')].filter((b) => b.getBoundingClientRect().height > 0); return { viewer: [Math.round(v.width), Math.round(v.height)], overflowX: document.documentElement.scrollWidth > innerWidth, inspectorVisible: !!document.querySelector('#inspector:not([hidden])'), visibleItems: items.length }; })())`,
      ),
    );
    report.dom[`unstaged_${w}x${h}`] = geometry;
    assert.equal(geometry.overflowX, false);
    await shot(`working-unstaged-${w}x${h}`);
  }
  await resize(1440, 900);
  await ev(
    `document.querySelector('.diff-viewer').dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true })); true`,
  );
  await waitFor('!document.querySelector(".diff-viewer")');
  report.checks.focus_returns_to_local_file = await ev(
    `document.activeElement?.title.split(' · ')[0] === 'src/app.rs' && !!document.activeElement.closest('#inspector-changes')`,
  );
  assert.ok(report.checks.focus_returns_to_local_file);
  await shot('working-panel-1440x900');

  // 3. Binary, soft gate and thousands of lines (untracked, read without Git).
  await evAsync(open('untracked', 'assets/novo.bin'));
  assert.ok(
    await ev(
      `document.querySelector('.diff-state')?.textContent.includes('Arquivo binário')`,
    ),
  );
  await evAsync(open('untracked', 'dados/registro.log'));
  assert.ok(
    await ev(
      `document.querySelector('.diff-state')?.textContent.includes('Arquivo grande')`,
    ),
  );
  await shot('working-large-gate-1440x900');
  report.latency_ms.untracked_3mib_after_confirm = await evAsync(
    `window.__timed(() => [...document.querySelectorAll('.diff-state .text-button')].find((b) => b.textContent.includes('Abrir mesmo assim')).click(), () => !!document.querySelector('.diff-row'))`,
  );
  report.latency_ms.untracked_12k_open = await evAsync(
    open('untracked', 'dados/tabela.csv'),
  );
  assert.equal(
    await ev(`+document.querySelector('.diff-body').dataset.totalLines`),
    12_001,
  );
  report.dom.rows_12k = await ev(
    `document.querySelectorAll('.diff-row').length`,
  );
  assert.ok(
    report.dom.rows_12k <= Math.ceil(900 / 20) + 81,
    `DOM ${report.dom.rows_12k}`,
  );
  report.latency_ms.untracked_12k_jump_to_end = await evAsync(
    `window.__timed(() => { const b = document.querySelector('.diff-body'); window.__scroll(b, b.scrollHeight); }, () => !!document.querySelector('.diff-row[data-index="12000"]'))`,
  );
  assert.equal(
    await ev(
      `document.querySelector('.diff-row[data-index="12000"] .tx').textContent`,
    ),
    tableLine(11_999),
  );
  await shot('working-untracked-12k-1440x900');
  report.memory_mib.after_12k_untracked = memory();
  assert.deepEqual(fingerprint(), before, 'inspection changed the repository');
  report.checks.repository_unchanged_by_inspection = true;

  // 4. External edit between pages: the next page request is refused, the list
  // is re-read and the viewer reopens on the new content (no mixed versions).
  await ev(`window.__scroll(document.querySelector('.diff-body'), 0); true`);
  await waitFor(`!!document.querySelector('.diff-row[data-index="1"]')`);
  const revision = await ev('window.__revision()');
  appendFileSync(join(repo, 'dados/tabela.csv'), 'linha externa\n');
  report.latency_ms.edit_detected_on_next_page = await evAsync(
    `window.__timed(() => window.__scroll(document.querySelector('.diff-body'), 6000 * 20), () => window.__revision() !== ${JSON.stringify(revision)} && document.querySelector('.diff-body')?.dataset.totalLines === '12002')`,
  );
  await evAsync(
    `window.__timed(() => { const b = document.querySelector('.diff-body'); window.__scroll(b, b.scrollHeight); }, () => !!document.querySelector('.diff-row[data-index="12001"]'))`,
  );
  assert.equal(
    await ev(
      `document.querySelector('.diff-row[data-index="12001"] .tx').textContent`,
    ),
    'linha externa',
  );
  report.checks.reopened_after_external_edit = true;

  // 5. Window focus re-reads the list; the open diff follows its file.
  await evAsync(open('unstaged', 'src/app.rs'));
  const beforeFocus = await ev('window.__revision()');
  appendFileSync(join(repo, 'src/app.rs'), 'fn externa() {}\n');
  report.latency_ms.focus_relist_and_reopen = await evAsync(
    `window.__timed(() => window.dispatchEvent(new Event('focus')), () => window.__revision() !== ${JSON.stringify(beforeFocus)} && [...document.querySelectorAll('.diff-row.kind-added .tx')].some((t) => t.textContent === 'fn externa() {}'))`,
  );
  assert.equal(
    await ev('window.__rendered()'),
    oraclePatch('unstaged', 'src/app.rs'),
  );
  // Unchanged repository: focus keeps the revision and the open diff.
  const stable = await ev('window.__revision()');
  await ev(`window.dispatchEvent(new Event('focus')); true`);
  await pause(400);
  assert.equal(await ev('window.__revision()'), stable);
  assert.ok(await ev(`!!document.querySelector('.diff-row')`));
  report.checks.focus_without_changes_keeps_revision = true;

  // 6. External `git add` stages the open file entirely: it leaves the group.
  git('add', 'src/app.rs');
  await evAsync(
    `window.__timed(() => window.dispatchEvent(new Event('focus')), () => !document.querySelector('.diff-viewer') && !window.__item('unstaged', 'src/app.rs'))`,
  );
  report.checks.notice = await ev(
    `document.querySelector('#inspector-changes .limit-note[role=status]')?.textContent.trim()`,
  );
  assert.ok(report.checks.notice?.includes('src/app.rs'));
  await shot('working-file-left-group-1440x900');
  const afterExternal = fingerprint();

  // 7. Commit diffs still work; the graph is intact after the overlay.
  await ev(`document.querySelector('#tab-commit').click(); true`);
  report.dom.commit_files_before_click = await ev(
    `[...document.querySelectorAll('#inspector-commit .file-item')].map((b) => b.title)`,
  );
  await evAsync(
    `window.__timed(() => [...document.querySelectorAll('#inspector-commit .file-item')].find((b) => b.title === 'src/app.rs').click(), () => document.querySelector('.diff-range')?.textContent.replace(/\\s+/g, ' ').includes('árvore vazia') && !!document.querySelector('.diff-row'))`,
  );
  await ev(
    `document.querySelector('.diff-viewer').dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true })); true`,
  );
  await waitFor('!document.querySelector(".diff-viewer")');
  report.checks.commit_diff_still_works = true;
  const graph = JSON.parse(
    await ev(
      `JSON.stringify((() => { const row = document.querySelector('.commit-row'); return { height: Math.round(row.getBoundingClientRect().height), drift: Math.abs(row.getBoundingClientRect().top - row.querySelector('svg').getBoundingClientRect().top), inert: document.querySelector('.history-host').inert }; })())`,
    ),
  );
  assert.equal(graph.height, 30);
  assert.ok(graph.drift < 1);
  assert.equal(graph.inert, false);
  report.checks.graph_after_diff = graph;
  report.memory_mib.final = memory();
  assert.deepEqual(
    fingerprint(),
    afterExternal,
    'the app changed the repository',
  );
  report.checks.repository_unchanged_after_external_edits = true;
  report.pass = true;
  writeFileSync(
    join(outDir, 'working-smoke.json'),
    JSON.stringify(report, null, 2) + '\n',
  );
  console.log(JSON.stringify(report, null, 2));
} finally {
  inspector?.close();
  app.kill('SIGTERM');
}
