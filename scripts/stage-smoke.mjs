// Real Tauri stage/unstage, trust, partial staging, refusals and failures (docs/17).
// Disposable fixture only. Usage: node scripts/stage-smoke.mjs <runtime-bench binary> <output-dir>
// Hyprland >= 0.55: the window floats in the hidden `special:poseidon` workspace;
// scroll and focus events are dispatched explicitly (no frame callbacks there).
// Reported write timings come from Rust (Git + reconciliation), excluding paint.
import { spawn, spawnSync } from 'node:child_process';
import {
  mkdtempSync,
  mkdirSync,
  writeFileSync,
  readFileSync,
  readdirSync,
  rmSync,
} from 'node:fs';
import { join, relative } from 'node:path';
import { tmpdir } from 'node:os';
import { createHash } from 'node:crypto';
import assert from 'node:assert/strict';
import { connectInspector } from './webkit-inspector.mjs';

const [binary, outDir] = process.argv.slice(2);
if (!binary || !outDir)
  throw new Error('Usage: stage-smoke.mjs <binary> <output-dir>');
mkdirSync(outDir, { recursive: true });
const pause = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const root = mkdtempSync(join(tmpdir(), 'poseidon-stage-'));
const repo = join(root, 'stage-demo');
mkdirSync(repo);
const env = Object.fromEntries(
  Object.entries(process.env).filter(([key]) => !key.startsWith('GIT_')),
);
Object.assign(env, {
  HOME: root,
  XDG_CONFIG_HOME: join(root, 'xdg'),
  XDG_DATA_HOME: join(root, 'app-data'),
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

// --- Disposable fixture ----------------------------------------------------
git('init', '-q', '--initial-branch=main');
write('alpha.txt', 'one\ntwo\n');
write('partial.txt', 'one\ntwo\nthree\n');
write('.gitattributes', '*.fail filter=reject\n');
git('add', '-A');
git(
  '-c',
  'user.name=Stage Smoke',
  '-c',
  'user.email=smoke@example.invalid',
  'commit',
  '-q',
  '-m',
  'Base',
);
write('alpha.txt', 'one\nTWO\n');
write('partial.txt', 'ONE\ntwo\nthree\n');
git('add', 'partial.txt');
write('partial.txt', 'ONE\ntwo\nTHREE\n');
write('novo 日本.txt', 'new\n');
write('blocked.fail', 'failed filter\n');
const indexBytes = () =>
  readFileSync(join(repo, '.git/index')).toString('base64');
const originalIndex = indexBytes();
const worktree = () =>
  Object.fromEntries(
    Object.entries(fingerprint()).filter(([p]) => !p.startsWith('.git/')),
  );
const originalTree = worktree();
// --- App -------------------------------------------------------------------
const endpoint = 'http://127.0.0.1:9237';
const app = spawn(binary, [], {
  env: { ...env, WEBKIT_INSPECTOR_HTTP_SERVER: '127.0.0.1:9237' },
  stdio: 'ignore',
});
let inspector;
const hypr = (lua) =>
  assert.equal(
    spawnSync('hyprctl', ['dispatch', lua], { encoding: 'utf8' }).stdout.trim(),
    'ok',
  );
const stats = (values) => {
  const sorted = [...values].sort((a, b) => a - b);
  return {
    samples: sorted.length,
    p50: sorted[Math.ceil(sorted.length * 0.5) - 1],
    p95: sorted[Math.ceil(sorted.length * 0.95) - 1],
    max: sorted.at(-1),
  };
};
const report = { checks: {}, latency_ms: {} };
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
    throw new Error(
      'UI condition failed: ' +
        label +
        '; ' +
        JSON.stringify(
          await ev(
            `({ text:document.querySelector('#inspector-changes')?.textContent, ctx:window.__ctx,last:window.__last, descriptor:Object.getOwnPropertyDescriptor(window.__TAURI_INTERNALS__,'invoke'),requests:window.__requests })`,
          ),
        ),
    );
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
    'document.querySelector(".primary-button") && !document.querySelector(".primary-button").disabled',
  );
  await ev(`window.__ctx = null; window.__last = null; window.__writes = []; window.__requests = [];
    const invoke = window.__TAURI_INTERNALS__.invoke.bind(window.__TAURI_INTERNALS__);
    window.__ipc = invoke;
    window.__capture = (cmd, args, r) => {
      if (cmd === 'get_working_changes') { window.__ctx = args; window.__last = r.data; }
      if (cmd === 'stage_file' || cmd === 'unstage_file') { window.__last = r.data.working; window.__writes.push({ cmd, gitMs:r.data.gitMs, relistMs:r.data.relistMs, elapsedMs:r.data.elapsedMs, incremental:r.data.incremental }); }
    };
    window.__item = (area,path) => [...(document.querySelector('#group-'+area)?.closest('section').querySelectorAll('.file-item') ?? [])].find(b=>b.title.split(' · ')[0] === path);
    true`);
  await ev(
    `window.__o=window.fetch; window.fetch=(u,o)=>decodeURIComponent(String(u)).includes('plugin:dialog|open')?Promise.resolve(new Response(${JSON.stringify(JSON.stringify(repo))},{headers:{'Tauri-Response':'ok','Content-Type':'application/json'}})):window.__o(u,o).then(async r=>{ const cmd=decodeURIComponent(String(u)).split('/').at(-1); if (r.headers.get('Tauri-Response') === 'ok' && ['get_working_changes','stage_file','unstage_file'].includes(cmd)) window.__capture(cmd,JSON.parse(o.body),await r.clone().json()); return r; }); document.querySelector('.primary-button').click(); true`,
  );
  await waitFor('document.querySelectorAll(".commit-row").length === 1');
  await ev(`document.querySelector('#tab-changes').click(); true`);
  await waitFor(
    `!!window.__item('unstaged','alpha.txt') && window.__last !== null`,
  );
  const act = (area, path) =>
    ev(
      `window.__item(${JSON.stringify(area)},${JSON.stringify(path)}).closest('.file-row').querySelector('.row-action').click(); true`,
    );
  const open = async (area, path) => {
    await ev(
      `window.__item(${JSON.stringify(area)},${JSON.stringify(path)}).click(); true`,
    );
    await waitFor(
      `document.querySelector('.diff-viewer h2')?.title === ${JSON.stringify(path)} && !!document.querySelector('.diff-row')`,
    );
  };
  const msg = () =>
    ev(`document.querySelector('.index-message')?.textContent.trim()`);
  const done = () =>
    waitFor(
      `!!document.querySelector('.index-message') && !document.querySelector('.changes-panel')?.getAttribute('aria-busy')?.includes('true') && !document.querySelector('.file-row.busy')`,
    );
  const cancel = () =>
    ev(`document.querySelector('.index-dialog .toolbar-button').click(); true`);
  const confirm = () =>
    ev(`document.querySelector('.index-dialog .primary-button').click(); true`);

  // Backend trust check even when an IPC caller bypasses the UI.
  const refused = await evAsync(
    `window.__ipc('stage_file', {...window.__ctx, revision: window.__last.revision, area:'unstaged', index: window.__last.unstaged.find(f=>f.path==='alpha.txt').index}).then(()=>null,e=>e.code)`,
  );
  assert.equal(refused, 'TrustRequired');
  assert.equal(indexBytes(), originalIndex);
  report.checks.backend_requires_trust = true;

  await act('unstaged', 'alpha.txt');
  await waitFor('!!document.querySelector(".index-dialog[open]")');
  assert.ok(
    await ev(`document.activeElement?.textContent.trim() === 'Cancelar'`),
  );
  await shot('01-trust-dialog');
  await cancel();
  assert.equal(indexBytes(), originalIndex);
  assert.deepEqual(worktree(), originalTree);
  report.checks.trust_cancel_no_write = true;
  await act('unstaged', 'alpha.txt');
  await confirm();
  await waitFor(
    `document.querySelector('.trust-bar')?.classList.contains('trusted') && !document.querySelector('.row-action:disabled')`,
  );
  assert.equal(
    indexBytes(),
    originalIndex,
    'grant must not replay an old selection',
  );
  report.checks.grant_relists_without_writing = true;

  // Modified → Stage → staged diff → Unstage; selected open file follows groups.
  await open('unstaged', 'alpha.txt');
  await shot('02-modified-diff');
  await act('unstaged', 'alpha.txt');
  await done();
  await waitFor(
    `!!window.__item('staged','alpha.txt') && document.querySelector('.diff-range')?.textContent.includes('→ index') && !!document.querySelector('.diff-row')`,
  );
  assert.equal(git('show', ':alpha.txt'), 'one\nTWO\n');
  assert.ok(
    await ev(
      `document.activeElement?.closest('.file-row')?.querySelector('.file-item').title === 'alpha.txt'`,
    ),
  );
  await shot('03-staged-diff');
  await act('staged', 'alpha.txt');
  await done();
  await waitFor(
    `!!window.__item('unstaged','alpha.txt') && document.querySelector('.diff-range')?.textContent.includes('→ working tree') && !!document.querySelector('.diff-row')`,
  );
  assert.equal(git('show', ':alpha.txt'), 'one\ntwo\n');
  assert.deepEqual(worktree(), originalTree);
  await shot('04-unstaged-preserved');
  report.checks.stage_diff_unstage_preserves_worktree = true;

  // Partial staging cancellation, confirmation, and stale confirmation.
  await act('unstaged', 'partial.txt');
  await waitFor(
    `document.querySelector('.index-dialog h2')?.textContent.includes('Substituir')`,
  );
  await shot('05-partial-confirmation');
  const partialIndex = indexBytes();
  await cancel();
  assert.equal(indexBytes(), partialIndex);
  await act('unstaged', 'partial.txt');
  write('partial.txt', 'ONE\ntwo\nTHREE\nfour\n');
  await confirm();
  await done();
  assert.ok((await msg()).includes('Nenhuma tentativa automática'));
  assert.equal(indexBytes(), partialIndex);
  report.checks.stale_confirmation_refused = true;
  await act('unstaged', 'partial.txt');
  await confirm();
  await done();
  assert.equal(
    git('show', ':partial.txt'),
    readFileSync(join(repo, 'partial.txt'), 'utf8'),
  );
  assert.ok(!(await ev(`!!window.__item('unstaged','partial.txt')`)));
  report.checks.partial_cancel_and_replace = true;

  // Lock owned by another process must survive; no retry when it is removed.
  writeFileSync(join(repo, '.git/index.lock'), 'external lock');
  const lockedIndex = indexBytes();
  await act('unstaged', 'alpha.txt');
  await done();
  assert.ok((await msg()).includes('index.lock'));
  assert.equal(
    readFileSync(join(repo, '.git/index.lock'), 'utf8'),
    'external lock',
  );
  assert.equal(indexBytes(), lockedIndex);
  await shot('06-index-lock-error');
  rmSync(join(repo, '.git/index.lock'));
  report.checks.lock_preserved_no_retry = true;

  // Real Git failure: clean filter introduced after listing, on an untracked file.
  git('config', 'filter.reject.clean', 'false');
  git('config', 'filter.reject.required', 'true');
  const failedIndex = indexBytes();
  await act('untracked', 'blocked.fail');
  await done();
  assert.ok((await msg()).includes('recusou'));
  assert.equal(indexBytes(), failedIndex);
  await shot('07-filter-failure');
  report.checks.filter_failure_reconciled = true;

  // Literal Unicode path, keyboard s/u, and invalidation across repeated cycles.
  await open('untracked', 'novo 日本.txt');
  await ev(
    `window.__item('untracked','novo 日本.txt').focus(); window.__item('untracked','novo 日本.txt').dispatchEvent(new KeyboardEvent('keydown',{key:'s',bubbles:true})); true`,
  );
  await done();
  assert.equal(git('show', ':novo 日本.txt'), 'new\n');
  await ev(
    `window.__item('staged','novo 日本.txt').dispatchEvent(new KeyboardEvent('keydown',{key:'u',bubbles:true})); true`,
  );
  await done();
  assert.ok(await ev(`!!window.__item('untracked','novo 日本.txt')`));
  report.checks.untracked_unicode_keyboard_roundtrip = true;
  for (let i = 0; i < 8; i++) {
    await act('unstaged', 'alpha.txt');
    await done();
    await act('staged', 'alpha.txt');
    await done();
  }
  report.checks.repeated_cycles = 8;
  report.latency_ms.writes = await ev('window.__writes');
  for (const cmd of ['stage_file', 'unstage_file']) {
    report.latency_ms[cmd] = stats(
      report.latency_ms.writes
        .filter((w) => w.cmd === cmd)
        .map((w) => w.elapsedMs),
    );
  }
  // Revocation is persisted by backend and blocks even direct IPC.
  await ev(`document.querySelector('.trust-bar .text-button').click(); true`);
  await waitFor(
    `document.querySelector('.trust-bar') && !document.querySelector('.trust-bar.trusted') && !document.querySelector('.row-action:disabled')`,
  );
  const revoked = await evAsync(
    `window.__ipc('unstage_file', {...window.__ctx, revision:window.__last.revision,index:0}).then(()=>null,e=>e.code)`,
  );
  assert.equal(revoked, 'TrustRequired');
  await shot('08-revoked');
  report.checks.revocation_enforced_by_backend = true;
  report.pass = true;
  writeFileSync(
    join(outDir, 'stage-smoke.json'),
    JSON.stringify(report, null, 2) + '\n',
  );
  console.log(JSON.stringify(report, null, 2));
} finally {
  inspector?.close();
  app.kill('SIGTERM');
  await pause(500);
  if (app.exitCode === null && app.signalCode === null) app.kill('SIGKILL');
  rmSync(root, { recursive: true, force: true });
}
