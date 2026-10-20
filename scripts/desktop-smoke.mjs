// Opt-in smoke test against a real DEBUG Tauri/WebKitGTK window.
// Only the folder-picker response is substituted; all Git/IPC calls are real.
import { mkdtemp, writeFile, readFile, readdir, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, relative } from 'node:path';
import { spawnSync } from 'node:child_process';
import assert from 'node:assert/strict';

const endpoint = process.env.POSEIDON_INSPECTOR_URL ?? 'http://127.0.0.1:9223';
const root = await mkdtemp(join(tmpdir(), 'poseidon-desktop-smoke-'));
let socket;
let targetId;
let nextId = 1;
const pending = new Map();

function git(...args) {
  const env = Object.fromEntries(
    Object.entries(process.env).filter(([key]) => !key.startsWith('GIT_')),
  );
  const output = spawnSync('git', args, {
    cwd: root,
    env: {
      ...env,
      GIT_CONFIG_NOSYSTEM: '1',
      GIT_CONFIG_GLOBAL: join(root, 'no-global-config'),
      GIT_AUTHOR_DATE: '2025-01-01T12:00:00Z',
      GIT_COMMITTER_DATE: '2025-01-01T12:00:00Z',
    },
    encoding: 'utf8',
  });
  assert.equal(output.status, 0, output.stderr);
  return output.stdout.trim();
}

async function fingerprint(path = root, files = {}) {
  for (const entry of await readdir(path, { withFileTypes: true })) {
    const child = join(path, entry.name);
    if (entry.isDirectory()) await fingerprint(child, files);
    else if (entry.isFile())
      files[relative(root, child)] = (await readFile(child)).toString('base64');
  }
  return files;
}

function send(method, params = {}) {
  return new Promise((resolve, reject) => {
    const id = nextId++;
    const timer = setTimeout(() => {
      pending.delete(id);
      reject(new Error(`Inspector timeout: ${method}`));
    }, 20000);
    pending.set(id, { resolve, reject, timer });
    const message = { id, method, params };
    socket.send(
      JSON.stringify(
        targetId && !method.startsWith('Target.')
          ? {
              id: nextId++,
              method: 'Target.sendMessageToTarget',
              params: { targetId, message: JSON.stringify(message) },
            }
          : message,
      ),
    );
  });
}

async function evaluate(expression) {
  const response = await send('Runtime.evaluate', {
    expression,
    returnByValue: true,
    awaitPromise: true,
  });
  if (response.wasThrown) throw new Error(JSON.stringify(response.result));
  return response.result?.value;
}

async function waitFor(expression) {
  for (let attempt = 0; attempt < 100; attempt++) {
    if (await evaluate(expression)) return;
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  const diagnostic = await evaluate(
    `JSON.stringify({rows:document.querySelectorAll('.commit-row').length,error:document.querySelector('.error-banner')?.textContent,detailError:document.querySelector('.detail-error')?.textContent,status:document.querySelector('.statusbar')?.textContent})`,
  );
  throw new Error(`UI condition timed out: ${expression}; ${diagnostic}`);
}

try {
  git('init', '--initial-branch=main');
  git('config', 'user.name', 'Desktop Smoke');
  git('config', 'user.email', 'smoke@example.invalid');
  git('config', 'commit.gpgsign', 'false');
  await writeFile(join(root, 'hello.txt'), 'first\n');
  git('add', '--all');
  git('commit', '-m', 'Primeiro commit real\n\nMensagem do smoke desktop.');
  await writeFile(join(root, 'hello.txt'), 'second\n');
  git('add', '--all');
  git('commit', '-m', 'Segundo commit real');
  git('branch', 'feature/desktop');
  git('update-ref', 'refs/remotes/origin/main', git('rev-parse', 'HEAD'));
  await writeFile(join(root, 'hello.txt'), 'working tree\n');
  await writeFile(join(root, 'new file.txt'), 'untracked\n');
  const before = await fingerprint();
  const listing = await (await fetch(endpoint)).text();
  const target = listing.match(/\/socket\/\d+\/\d+\/WebPage/);
  assert.ok(
    target,
    'No inspectable WebKit page. Start the DEBUG app with WEBKIT_INSPECTOR_HTTP_SERVER.',
  );
  socket = new WebSocket(`${endpoint.replace('http:', 'ws:')}${target[0]}`);
  function receive(message) {
    if (message.method === 'Target.targetCreated')
      targetId = message.params.targetInfo.targetId;
    if (message.method === 'Target.dispatchMessageFromTarget')
      return receive(JSON.parse(message.params.message));
    if (!message.id) return;
    const waiter = pending.get(message.id);
    if (!waiter) return;
    pending.delete(message.id);
    clearTimeout(waiter.timer);
    if (message.error) waiter.reject(new Error(JSON.stringify(message.error)));
    else waiter.resolve(message.result);
  }
  socket.onmessage = ({ data }) => receive(JSON.parse(data));
  await new Promise((resolve, reject) => {
    socket.onopen = resolve;
    socket.onerror = reject;
  });
  for (let i = 0; i < 50 && !targetId; i++)
    await new Promise((resolve) => setTimeout(resolve, 50));
  assert.ok(targetId, 'No WebKit page target');
  await send('Runtime.enable');
  await evaluate('window.__poseidonReloading = true; location.reload(); true');
  await waitFor(
    "window.__poseidonReloading === undefined && document.readyState === 'complete'",
  );
  await waitFor(
    'document.querySelector(".primary-button") && !document.querySelector(".primary-button").disabled',
  );
  // This hook exists only in this external test, never in the shipped bundle.
  await evaluate(`window.__poseidonOriginalInvoke = window.__TAURI_INTERNALS__.invoke;
    window.__poseidonOriginalFetch = window.fetch;
    window.__poseidonPickerPath = ${JSON.stringify(root)};
    window.fetch = (url, options) => decodeURIComponent(String(url)).includes('plugin:dialog|open')
      ? Promise.resolve(new Response(JSON.stringify(window.__poseidonPickerPath), {headers:{'Tauri-Response':'ok','Content-Type':'application/json'}}))
      : window.__poseidonOriginalFetch(url, options);
    document.querySelector('.primary-button').click(); true`);
  await waitFor(
    'document.querySelectorAll(".commit-row").length === 2 && document.querySelector(".commit-message")',
  );
  assert.equal(
    await evaluate('document.querySelector("h1").textContent'),
    root.split('/').at(-1),
  );
  assert.ok(
    (
      await evaluate('document.querySelector(".branches-section").textContent')
    ).includes('feature/desktop'),
  );
  assert.ok(
    (
      await evaluate('document.querySelector(".branches-section").textContent')
    ).includes('origin/main'),
  );
  // Local changes are listed on demand, when their tab is shown.
  await evaluate('document.querySelector("#tab-changes").click(); true');
  await waitFor(
    '[...document.querySelectorAll("#inspector-changes .file-item")].some((b) => b.title === "new file.txt")',
  );
  await evaluate('document.querySelector("#tab-commit").click(); true');
  await evaluate('document.querySelectorAll(".commit-row")[1].click(); true');
  await waitFor(
    'document.querySelector(".commit-message")?.textContent.includes("Mensagem do smoke desktop.")',
  );
  await evaluate(
    'document.querySelector("input[type=search]").value = "Segundo"; document.querySelector("input[type=search]").dispatchEvent(new Event("input", {bubbles:true})); true',
  );
  await waitFor(
    'document.querySelectorAll(".commit-row.search-match").length === 1',
  );
  assert.equal(
    await evaluate('document.querySelectorAll(".commit-row").length'),
    2,
  );
  await evaluate(
    'document.querySelector("input[type=search]").value = ""; document.querySelector("input[type=search]").dispatchEvent(new Event("input", {bubbles:true})); true',
  );
  await waitFor('document.querySelectorAll(".commit-row").length === 2');
  const metrics = await evaluate(`JSON.stringify({
    viewport: [innerWidth, innerHeight],
    horizontalOverflow: document.documentElement.scrollWidth > innerWidth,
    rows: document.querySelectorAll('.commit-row').length,
    status: document.querySelector('.statusbar').textContent.trim()
  })`);
  assert.equal(JSON.parse(metrics).horizontalOverflow, false);
  const { viewport } = JSON.parse(metrics);
  const picture = await send('Page.snapshotRect', {
    x: 0,
    y: 0,
    width: viewport[0],
    height: viewport[1],
    coordinateSystem: 'Viewport',
  });
  await writeFile(
    join(tmpdir(), 'poseidon-desktop-smoke.png'),
    Buffer.from(picture.dataURL.split(',')[1], 'base64'),
  );
  await evaluate(`window.__poseidonPickerPath = ${JSON.stringify(join(root, 'missing'))};
    document.querySelector('.add-repo').click(); true`);
  await waitFor('!!document.querySelector(".error-banner")');
  assert.equal(
    await evaluate('document.querySelectorAll(".commit-row").length'),
    2,
  );
  const errorCode = await evaluate(`window.__poseidonError = null;
    window.__poseidonOriginalInvoke('get_commit', {sessionId: 999999, generation: 1, oid: '--exec=bad'})
    .catch(error => { window.__poseidonError = error.code }); true`);
  void errorCode;
  await waitFor('window.__poseidonError === "StaleSession"');
  assert.deepEqual(
    await fingerprint(),
    before,
    'A read changed files in the Git fixture',
  );
  console.log(
    'PASS: real WebKit/Tauri IPC, open, 2 commits, local/remote branches, file changes, selected message, search, invalid-folder preservation, stale-session rejection and byte-for-byte fixture preservation.',
  );
  console.log(metrics);
} finally {
  if (socket?.readyState === WebSocket.OPEN) {
    try {
      await evaluate(
        'if (window.__poseidonOriginalFetch) window.fetch = window.__poseidonOriginalFetch; true',
      );
    } catch {}
    socket.close();
  }
  for (const waiter of pending.values()) clearTimeout(waiter.timer);
  assert.ok(root.startsWith(join(tmpdir(), 'poseidon-desktop-smoke-')));
  await rm(root, { recursive: true, force: true });
}
