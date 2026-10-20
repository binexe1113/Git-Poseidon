// Real-window layout inspection for the visual comparison fixture (docs/15).
// Usage: node scripts/ui-layout-check.mjs <binary> <output-dir> <label>
// Requires a `runtime-bench` release (inspector) and Hyprland >= 0.55 (Lua
// dispatchers). The window is floated into the hidden `special:poseidon`
// workspace so it never covers the user's windows; captures come from WebKit's
// own renderer (Page.snapshotRect), not from the compositor.
import { spawn, spawnSync } from 'node:child_process';
import { mkdir, readFile, readdir, writeFile } from 'node:fs/promises';
import { join, relative } from 'node:path';
import { createHash } from 'node:crypto';
import assert from 'node:assert/strict';
import { connectInspector } from './webkit-inspector.mjs';

const [binary, outDir, label] = process.argv.slice(2);
if (!binary || !outDir || !label)
  throw new Error('Usage: ui-layout-check.mjs <binary> <output-dir> <label>');
const endpoint = 'http://127.0.0.1:9232';
const sizes = [
  [1280, 720],
  [1366, 768],
  [1440, 900],
  [1920, 1080],
  [1000, 900],
];
const pause = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
let app, inspector;

function hypr(lua) {
  const result = spawnSync('hyprctl', ['dispatch', lua], { encoding: 'utf8' });
  assert.equal(result.stdout.trim(), 'ok', `${lua}: ${result.stdout}`);
}
async function fingerprint(root, path = root, files = {}) {
  for (const entry of await readdir(path, { withFileTypes: true })) {
    const next = join(path, entry.name);
    if (entry.isDirectory()) await fingerprint(root, next, files);
    else if (entry.isFile())
      files[relative(root, next)] = createHash('sha256')
        .update(await readFile(next))
        .digest('hex');
  }
  return files;
}
function memory(samples = 20) {
  const p = spawnSync(
    'python3',
    [
      'scripts/measure-linux-memory.py',
      String(app.pid),
      '--samples',
      String(samples),
      '--interval',
      '0.25',
    ],
    { encoding: 'utf8' },
  );
  assert.equal(p.status, 0, p.stderr);
  return JSON.parse(p.stdout);
}

// Geometry is measured from what is actually hit-testable in the viewport:
// a region that only exists below the fold does not count as visible.
const measure = `(() => {
  const visible = (el) => {
    if (!el) return false;
    const r = el.getBoundingClientRect();
    if (r.width < 1 || r.height < 1 || r.bottom <= 0 || r.top >= innerHeight) return false;
    const x = Math.min(innerWidth - 1, Math.max(0, r.left + Math.min(r.width / 2, 40)));
    const y = Math.min(innerHeight - 1, Math.max(0, r.top + Math.min(r.height / 2, 10)));
    const hit = document.elementFromPoint(x, y);
    return !!hit && (hit === el || el.contains(hit));
  };
  const list = document.querySelector('.history-list');
  const box = list.getBoundingClientRect();
  const rows = [...document.querySelectorAll('.commit-row')];
  const rects = rows.map((row) => [+row.dataset.index, row.getBoundingClientRect()]).sort((a, b) => a[0] - b[0]);
  const step = rects.length > 1 ? rects[1][1].top - rects[0][1].top : 0;
  const full = rects.filter(([, r]) => r.top >= box.top - 0.5 && r.bottom <= box.bottom + 0.5 && r.bottom <= innerHeight).length;
  const selected = document.querySelector('.commit-row.selected');
  const subject = selected?.querySelector('.commit-subject strong');
  const heading = document.querySelector('.commit-detail h3');
  const message = document.querySelector('.commit-message');
  const main = document.querySelector('main');
  const svgDrift = Math.max(0, ...rows.map((row) => Math.abs(row.getBoundingClientRect().top - row.querySelector('svg').getBoundingClientRect().top)));
  return JSON.stringify({
    viewport: [innerWidth, innerHeight],
    historyTop: Math.round(box.top),
    historyHeight: Math.round(Math.min(box.bottom, innerHeight) - Math.max(box.top, 0)),
    rowStep: Math.round(step * 10) / 10,
    fullRowsVisible: full,
    domRows: rows.length,
    subjectWidth: subject ? Math.round(subject.parentElement.getBoundingClientRect().width) : null,
    subjectFontPx: subject ? parseFloat(getComputedStyle(subject).fontSize) : null,
    detailHeadingVisible: visible(heading),
    detailHeadingTop: heading ? Math.round(heading.getBoundingClientRect().top) : null,
    messageVisible: visible(message),
    selectedRowVisible: visible(selected),
    mainScrolls: main.scrollHeight > main.clientHeight + 1,
    pageOverflowX: document.documentElement.scrollWidth > innerWidth,
    svgRowDriftPx: Math.round(svgDrift * 100) / 100,
  });
})()`;

try {
  await mkdir(outDir, { recursive: true });
  const fixtureInfo = JSON.parse(
    spawnSync('python3', ['scripts/fixtures/create-history.py'], {
      encoding: 'utf8',
    }).stdout,
  );
  const before = await fingerprint(fixtureInfo.repository);
  app = spawn(binary, [], {
    env: {
      ...process.env,
      WEBKIT_INSPECTOR_HTTP_SERVER: new URL(endpoint).host,
    },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  for (let i = 0; i < 100; i++) {
    const clients = spawnSync('hyprctl', ['clients', '-j'], {
      encoding: 'utf8',
    });
    if (JSON.parse(clients.stdout).some((c) => c.pid === app.pid)) break;
    assert.ok(i < 99, 'Window did not appear');
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
  const evaluate = inspector.evaluate;
  async function waitFor(expression) {
    for (let i = 0; i < 200; i++) {
      if (await evaluate(expression)) return;
      await pause(50);
    }
    throw new Error('UI condition failed: ' + expression);
  }
  async function snapshot(name) {
    const shot = await inspector.send('Page.snapshotRect', {
      x: 0,
      y: 0,
      width: await evaluate('innerWidth'),
      height: await evaluate('innerHeight'),
      coordinateSystem: 'Viewport',
    });
    await writeFile(
      join(outDir, `${label}-${name}.png`),
      Buffer.from(shot.dataURL.split(',')[1], 'base64'),
    );
  }
  async function resize(w, h) {
    hypr(`hl.dsp.window.resize({ x = ${w}, y = ${h}, window = "${window}" })`);
    await waitFor(`innerWidth === ${w} && innerHeight === ${h}`);
    await pause(400);
  }
  await resize(1440, 900);
  await waitFor(
    'document.querySelector(".primary-button") && !document.querySelector(".primary-button").disabled',
  );
  await pause(1500);
  const emptyMemory = memory();
  // Same technique as graph-smoke: only the folder-picker answer is replaced.
  await evaluate(
    `window.__original=window.fetch;window.fetch=(url,options)=>decodeURIComponent(String(url)).includes('plugin:dialog|open')?Promise.resolve(new Response(${JSON.stringify(JSON.stringify(fixtureInfo.repository))},{headers:{'Tauri-Response':'ok','Content-Type':'application/json'}})):window.__original(url,options);document.querySelector('.primary-button').click();true`,
  );
  await waitFor(
    'document.querySelectorAll(".commit-row").length > 0 && !!document.querySelector(".commit-message")',
  );
  const selectMerge = `(() => { const row = [...document.querySelectorAll('.commit-row')].find((r) => r.querySelector('.commit-subject strong')?.textContent === 'Merge fix/layout'); row.click(); return true; })()`;
  const mergeShown = `document.querySelector('.commit-row.selected .commit-subject strong')?.textContent === 'Merge fix/layout' && document.querySelector('.commit-message')?.textContent.includes('Merge fix/layout')`;
  const results = {};
  for (const [w, h] of sizes) {
    await resize(w, h);
    await evaluate(
      `document.querySelector('.history-list').scrollTop=0;document.querySelector('main').scrollTop=0;true`,
    );
    await pause(150);
    await evaluate(selectMerge);
    await waitFor(mergeShown);
    await pause(200);
    results[`${w}x${h}`] = JSON.parse(await evaluate(measure));
    await snapshot(`merge-${w}x${h}`);
  }
  // Resize back from narrow to wide: the selection must survive by OID.
  await resize(1440, 900);
  results.resizedBackKeepsSelection = await evaluate(mergeShown);
  await evaluate(`document.querySelector('.history-list').scrollTop=1e7;true`);
  await pause(300);
  results['1440x900-scrolled-end'] = JSON.parse(await evaluate(measure));
  await snapshot('scroll-end-1440x900');
  await evaluate(`document.querySelector('.history-list').scrollTop=0;true`);
  await pause(2000);
  const settledMemory = memory();
  assert.deepEqual(
    await fingerprint(fixtureInfo.repository),
    before,
    'Inspection changed fixture files',
  );
  const report = {
    label,
    binary,
    fixture: { path: fixtureInfo.repository, commits: fixtureInfo.commits },
    results,
    memory: { empty: emptyMemory, fixture_open_1440x900: settledMemory },
    note: 'runtime-bench release with inspector connected; window floated into a hidden Hyprland special workspace (no frame callbacks, so no RAF data). Captures via WebKit Page.snapshotRect. Fixture hashes unchanged.',
  };
  await writeFile(
    join(outDir, `${label}-layout.json`),
    JSON.stringify(report, null, 2) + '\n',
  );
  console.log(JSON.stringify(report, null, 2));
} finally {
  inspector?.close();
  if (app?.exitCode === null) {
    const done = new Promise((resolve) => app.once('exit', resolve));
    app.kill('SIGTERM');
    await Promise.race([done, pause(3000)]);
    if (app.exitCode === null && !app.signalCode) app.kill('SIGKILL');
  }
}
