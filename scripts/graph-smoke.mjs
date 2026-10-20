// Real Tauri + Git graph validation and interactive performance sampling.
import { spawn, spawnSync } from 'node:child_process';
import { readFile, readdir, writeFile } from 'node:fs/promises';
import { join, relative } from 'node:path';
import { createHash } from 'node:crypto';
import assert from 'node:assert/strict';
import { connectInspector } from './webkit-inspector.mjs';
const pause = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const endpoint = 'http://127.0.0.1:9225';
let inspector, app, fixture;
async function lineFrom(child, pattern) {
  let value = '',
    errors = '';
  child.stdout.on('data', (d) => {
    value += d;
  });
  child.stderr.on('data', (d) => {
    errors = (errors + d).slice(-4000);
  });
  for (let i = 0; i < 400; i++) {
    const match = value.match(pattern);
    if (match) return match;
    if (child.exitCode !== null) throw new Error(errors);
    await pause(25);
  }
  throw new Error('Process did not become ready: ' + errors);
}
async function stop(child) {
  if (!child?.pid || child.exitCode !== null || child.signalCode) return;
  const done = new Promise((resolve) => child.once('exit', resolve));
  child.kill('SIGTERM');
  await Promise.race([done, pause(3000)]);
  if (!child.signalCode && child.exitCode === null) {
    child.kill('SIGKILL');
    await done;
  }
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
function memory() {
  const p = spawnSync(
    'python3',
    ['scripts/measure-linux-memory.py', String(app.pid)],
    { encoding: 'utf8' },
  );
  assert.equal(p.status, 0, p.stderr);
  return JSON.parse(p.stdout);
}
function stats(values) {
  values.sort((a, b) => a - b);
  return {
    samples: values.length,
    p50: values[Math.ceil(values.length * 0.5) - 1],
    p95: values[Math.ceil(values.length * 0.95) - 1],
    max: values.at(-1),
  };
}
try {
  fixture = spawn('./target/release/examples/graph_fixture', ['10000'], {
    stdio: ['pipe', 'pipe', 'pipe'],
  });
  const fixtureInfo = JSON.parse((await lineFrom(fixture, /(\{[^\n]+\})/))[1]);
  const before = await fingerprint(fixtureInfo.path);
  const oracleProcess = spawnSync(
    'git',
    ['--no-pager', '--no-optional-locks', 'rev-list', '--all', '--parents'],
    {
      cwd: fixtureInfo.path,
      encoding: 'utf8',
      env: Object.fromEntries(
        Object.entries(process.env).filter(([key]) => !key.startsWith('GIT_')),
      ),
    },
  );
  assert.equal(oracleProcess.status, 0, oracleProcess.stderr);
  const oracle = new Map(
    oracleProcess.stdout
      .trim()
      .split('\n')
      .map((line) => {
        const [oid, ...parents] = line.split(' ');
        return [oid, parents];
      }),
  );
  let checkedSegments = 0;
  app = spawn(process.argv[2] ?? './target/release/git-poseidon', [], {
    env: {
      ...process.env,
      WEBKIT_INSPECTOR_HTTP_SERVER: new URL(endpoint).host,
    },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  await lineFrom(app, /POSEIDON_BENCH_READY_US=(\d+)/);
  inspector = await connectInspector(endpoint);
  await inspector.send('Runtime.enable');
  const evaluate = inspector.evaluate;
  async function waitFor(expression) {
    for (let i = 0; i < 300; i++) {
      if (await evaluate(expression)) return;
      await pause(50);
    }
    throw new Error(
      'UI condition failed: ' +
        expression +
        '; ' +
        (await evaluate(
          'document.querySelector(".history-error")?.textContent || document.querySelector(".error-banner")?.textContent',
        )),
    );
  }
  await waitFor(
    'document.querySelector(".primary-button") && !document.querySelector(".primary-button").disabled',
  );
  await pause(2000);
  const empty = memory();
  await evaluate(
    `window.__graphRequests=0;window.__graphOriginalFetch=window.fetch;window.fetch=(url,options)=>{const target=decodeURIComponent(String(url));if(target.includes('get_history'))window.__graphRequests++;return target.includes('plugin:dialog|open')?Promise.resolve(new Response(${JSON.stringify(JSON.stringify(fixtureInfo.path))},{headers:{'Tauri-Response':'ok','Content-Type':'application/json'}})):window.__graphOriginalFetch(url,options);};window.__graphOpenStart=performance.now();document.querySelector('.primary-button').click();true`,
  );
  await waitFor(
    'document.querySelectorAll(".graph-node").length > 0 && document.querySelector(".commit-message")',
  );
  const firstUseful = await evaluate(
    'performance.now()-window.__graphOpenStart',
  );
  const firstGeometry = await evaluate(
    'document.querySelector(".commit-row[data-index=\\\"0\\\"] .commit-graph").outerHTML',
  );
  assert.ok(await evaluate('!!document.querySelector(".ref-badge.current")'));
  await evaluate('document.querySelector(".commit-row").focus();true');
  const firstPage = memory();
  let secondGeometry;
  let maxDOM = 0,
    maxCache = 0;
  const pageTimes = [];
  for (let i = 0; i < 60; i++) {
    const state = await evaluate(
      `JSON.stringify({loaded:+document.querySelector('.history-list').dataset.loaded,cache:+document.querySelector('.history-list').dataset.cachedPages,dom:document.querySelectorAll('.commit-row').length,error:document.querySelector('.history-error')?.textContent})`,
    );
    const current = JSON.parse(state);
    maxDOM = Math.max(maxDOM, current.dom);
    maxCache = Math.max(maxCache, current.cache);
    assert.ok(!current.error, current.error);
    const segments = JSON.parse(
      await evaluate(
        `JSON.stringify([...document.querySelectorAll('.commit-graph path')].map(path=>[path.dataset.child,path.dataset.parent]))`,
      ),
    );
    for (const [child, parent] of segments) {
      assert.ok(
        oracle.get(child)?.includes(parent),
        'Rendered a non-parent edge',
      );
      checkedSegments++;
    }
    if (!secondGeometry && current.loaded >= 400)
      secondGeometry = await evaluate(
        'document.querySelector(".commit-row[data-index=\\\"200\\\"] .commit-graph")?.outerHTML',
      );
    if (current.loaded === 10000) break;
    assert.ok(i < 59, 'Traversal did not finish');
    await evaluate(
      `window.__graphPageStart=performance.now();document.querySelector('.history-list').scrollTop=(${current.loaded}-5)*+document.querySelector('.history-list').dataset.rowHeight;true`,
    );
    await waitFor(
      `+document.querySelector('.history-list').dataset.loaded > ${current.loaded}`,
    );
    pageTimes.push(await evaluate('performance.now()-window.__graphPageStart'));
  }
  // DOM stays proportional to the viewport: visible rows + 2×20 overscan + 2.
  const domBudget = await evaluate(
    `(()=>{const l=document.querySelector('.history-list');return Math.ceil(l.clientHeight/+l.dataset.rowHeight)+1+40+2})()`,
  );
  assert.ok(maxDOM <= domBudget, `DOM grew to ${maxDOM} (budget ${domBudget})`);
  assert.ok(maxCache <= 4, `Page cache grew to ${maxCache}`);
  const traversed = memory();
  const requestsBeforeIdle = await evaluate('window.__graphRequests');
  await pause(5000);
  const requestsAfterIdle = await evaluate('window.__graphRequests');
  const settled = memory();
  assert.ok(
    requestsAfterIdle - requestsBeforeIdle <= 1,
    'History kept querying while idle',
  );
  await inspector.send('Heap.gc');
  await pause(1000);
  const diagnosticGC = memory();
  // Page zero remained pinned by focus in the frontend, but the backend evicted it.
  await evaluate(
    'document.querySelector(".history-list").scrollTop=0;document.querySelector(".commit-row[data-index=\\\"0\\\"] .graph-node").dispatchEvent(new MouseEvent("click",{bubbles:true}));true',
  );
  await waitFor(
    '!!document.querySelector(".commit-message") && !document.querySelector(".detail-error")',
  );
  await evaluate(
    'document.querySelector(".history-list").scrollTop=195*+document.querySelector(".history-list").dataset.rowHeight;true',
  );
  await waitFor(
    '!!document.querySelector(".commit-row[data-index=\\\"199\\\"]") && !!document.querySelector(".commit-row[data-index=\\\"200\\\"]")',
  );
  assert.ok(secondGeometry, 'Page 200 was not sampled during traversal');
  assert.equal(
    await evaluate(
      'document.querySelector(".commit-row[data-index=\\\"200\\\"] .commit-graph").outerHTML',
    ),
    secondGeometry,
    'Replayed page 200 moved lanes',
  );
  const alignment = JSON.parse(
    await evaluate(
      `JSON.stringify([...document.querySelectorAll('.commit-row')].map(row=>({index:+row.dataset.index,y:row.getBoundingClientRect().top,svgY:row.querySelector('svg').getBoundingClientRect().top})))`,
    ),
  );
  for (const r of alignment)
    assert.ok(Math.abs(r.y - r.svgY) < 1, 'Graph shifted away from text row');
  const a = alignment.find((r) => r.index === 199),
    b = alignment.find((r) => r.index === 200);
  assert.equal(
    Math.round(b.y - a.y),
    await evaluate(
      `+document.querySelector('.history-list').dataset.rowHeight`,
    ),
  );
  await evaluate('document.querySelector(".history-list").scrollTop=0;true');
  await waitFor(
    '!!document.querySelector(".commit-row[data-index=\\\"0\\\"]")',
  );
  assert.equal(
    await evaluate(
      'document.querySelector(".commit-row[data-index=\\\"0\\\"] .commit-graph").outerHTML',
    ),
    firstGeometry,
    'Replayed page moved lanes',
  );
  // Select a real merge via its node and verify the corresponding message.
  await waitFor('!!document.querySelector(".ref-badge.tag")');
  assert.ok(await evaluate('!!document.querySelector(".ref-badge.tag")'));
  await evaluate(
    `window.__graphSelectedButton=document.querySelector('.merge-ring').closest('button');window.__graphSelectedOid=window.__graphSelectedButton.id;window.__graphSelectedSummary=window.__graphSelectedButton.querySelector('strong').textContent;window.__graphSelectedIndex=+window.__graphSelectedButton.dataset.index;window.__graphSelectedButton.querySelector('.graph-node').dispatchEvent(new MouseEvent('click',{bubbles:true}));true`,
  );
  await waitFor(
    `document.querySelector('.commit-row.selected')?.id===window.__graphSelectedOid && document.querySelector('.commit-message')?.textContent.includes(window.__graphSelectedSummary)`,
  );
  await evaluate(
    `window.__graphFrames=null;(async()=>{const intervals=[];const list=document.querySelector('.history-list');let previous=await new Promise(requestAnimationFrame);for(let i=0;i<180;i++){list.scrollTop=(i%150)*+list.dataset.rowHeight;const now=await new Promise(requestAnimationFrame);intervals.push(now-previous);previous=now;}window.__graphFrames=intervals;})();true`,
  );
  await waitFor('window.__graphFrames!==null');
  const frames = await evaluate('window.__graphFrames');
  await evaluate(
    'document.querySelector(".history-list").scrollTop=Math.max(0,window.__graphSelectedIndex-1)*+document.querySelector(".history-list").dataset.rowHeight;true',
  );
  await pause(200);
  const screenshot = await inspector.send('Page.snapshotRect', {
    x: 0,
    y: 0,
    width: await evaluate('innerWidth'),
    height: await evaluate('innerHeight'),
    coordinateSystem: 'Viewport',
  });
  await writeFile(
    '/tmp/poseidon-commit-graph.png',
    Buffer.from(screenshot.dataURL.split(',')[1], 'base64'),
  );
  assert.deepEqual(
    await fingerprint(fixtureInfo.path),
    before,
    'Reading graph changed fixture files',
  );
  const viewport = await evaluate('[innerWidth, innerHeight]');
  inspector.close();
  inspector = undefined;
  await pause(3000);
  const idleProcess = spawnSync(
    'python3',
    [
      'scripts/measure-linux-memory.py',
      String(app.pid),
      '--samples',
      '20',
      '--interval',
      '0.25',
    ],
    { encoding: 'utf8' },
  );
  assert.equal(idleProcess.status, 0, idleProcess.stderr);
  const disconnectedIdle = JSON.parse(idleProcess.stdout);
  console.log(
    JSON.stringify(
      {
        pass: true,
        commits: 10000,
        first_useful_poll_observed_ms: firstUseful,
        incremental_page_poll_observed_ms: stats(pageTimes),
        max_visible_rows: maxDOM,
        dom_row_budget: domBudget,
        viewport,
        max_frontend_pages: maxCache,
        idle_history_requests: requestsAfterIdle - requestsBeforeIdle,
        checked_svg_segments: checkedSegments,
        raf_intervals_ms: stats(frames),
        memory: {
          empty,
          first_page: firstPage,
          after_10000: traversed,
          after_5s_idle: settled,
          after_diagnostic_gc: diagnosticGC,
          final_idle_inspector_disconnected: disconnectedIdle,
        },
        note: 'Release runtime-bench on real WebKitGTK; inspector connected. Page latencies include 50ms polling. RAF intervals are observed callbacks, not compositor presentation times. RSS samples, not a peak profile. Fixtures unchanged.',
      },
      null,
      2,
    ),
  );
} finally {
  inspector?.close();
  await stop(app);
  if (fixture) {
    const done = new Promise((resolve) => fixture.once('exit', resolve));
    fixture.stdin.end();
    await Promise.race([done, pause(3000)]);
    await stop(fixture);
  }
}
