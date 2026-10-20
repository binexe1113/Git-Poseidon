// Dedicated opt-in release benchmark: no production UI hooks or mock IPC.
import { spawn } from 'node:child_process';
import { performance } from 'node:perf_hooks';
import assert from 'node:assert/strict';
const endpoint = process.env.POSEIDON_INSPECTOR_URL ?? 'http://127.0.0.1:9224';
const inspectorURL = new URL(endpoint);
assert.ok(
  inspectorURL.protocol === 'http:' &&
    ['127.0.0.1', 'localhost', '[::1]'].includes(inspectorURL.hostname),
  'Inspector must use a loopback HTTP endpoint',
);
const binary = process.argv[2] ?? './target/release/git-poseidon';
let socket, targetId, child;
let nextId = 1;
const pending = new Map();
const pause = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
function stats(values) {
  const s = [...values].sort((a, b) => a - b);
  return {
    samples: s.length,
    min: s[0],
    p50: s[Math.ceil(s.length * 0.5) - 1],
    p95: s[Math.ceil(s.length * 0.95) - 1],
    max: s.at(-1),
  };
}
async function launch() {
  const start = performance.now();
  child = spawn(binary, [], {
    env: {
      ...process.env,
      WEBKIT_INSPECTOR_HTTP_SERVER: new URL(endpoint).host,
    },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  let stdout = '',
    stderr = '';
  child.stdout.on('data', (data) => {
    stdout += data;
  });
  child.stderr.on('data', (data) => {
    stderr = (stderr + data).slice(-4000);
  });
  child.on('error', (error) => {
    stderr = error.message;
  });
  for (let i = 0; i < 400; i++) {
    const match = stdout.match(/POSEIDON_BENCH_READY_US=(\d+)/);
    if (match)
      return {
        rust_main_to_two_raf_ms: Number(match[1]) / 1000,
        parent_spawn_to_marker_ms: performance.now() - start,
      };
    if (child.exitCode !== null) throw new Error(`Desktop exited: ${stderr}`);
    await pause(25);
  }
  throw new Error(
    `No startup marker: compile with --features runtime-bench. ${stderr}`,
  );
}
async function stop() {
  if (!child?.pid || child.exitCode !== null || child.signalCode) return;
  const exit = new Promise((resolve) => child.once('exit', resolve));
  child.kill('SIGTERM');
  await Promise.race([exit, pause(3000)]);
  if (child.exitCode === null && !child.signalCode) {
    child.kill('SIGKILL');
    await exit;
  }
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

try {
  const startup = [];
  for (let i = 0; i < 12; i++) {
    const sample = await launch();
    if (i >= 2) startup.push(sample);
    if (i < 11) {
      await stop();
      await pause(150);
    }
  }
  const listing = await (await fetch(endpoint)).text();
  const target = listing.match(/\/socket\/\d+\/\d+\/WebPage/);
  assert.ok(target, 'No WebKit target');
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
  await evaluate(`window.__runtimeBench = null; (async () => {
    try {
      const samples = []; let responseBytes = 0, batchStart = 0;
      for(let i=0;i<220;i++) {
        if(i === 20) batchStart = performance.now();
        const start = performance.now();
        const reply = await window.__TAURI_INTERNALS__.invoke('runtime_probe',{sequence:i,ready:false});
        const elapsed = performance.now()-start;
        if(reply[0] !== i) throw new Error('Bad sequence');
        if(i>=20) samples.push(elapsed);
        responseBytes = Math.max(responseBytes,JSON.stringify(reply).length);
      }
      window.__runtimeBench = {samples,responseBytes,batchMs:performance.now()-batchStart};
    } catch(error) { window.__runtimeBench = {error:String(error)}; }
  })(); true`);
  let ipc;
  for (let i = 0; i < 200; i++) {
    ipc = await evaluate('window.__runtimeBench');
    if (ipc) break;
    await pause(50);
  }
  assert.ok(ipc && !ipc.error, JSON.stringify(ipc));
  socket.close();
  socket = undefined;
  // Give the inspector and startup work time to settle before idle sampling.
  await pause(3000);
  const memory = spawn('python3', [
    'scripts/measure-linux-memory.py',
    String(child.pid),
    '--samples',
    '20',
    '--interval',
    '0.25',
  ]);
  let memoryOutput = '';
  memory.stdout.on('data', (data) => {
    memoryOutput += data;
  });
  memory.stderr.pipe(process.stderr);
  const memoryCode = await new Promise((resolve) =>
    memory.once('exit', resolve),
  );
  assert.equal(memoryCode, 0);
  console.log(
    JSON.stringify(
      {
        mode: 'release runtime-bench; empty shell; warm launches; inspector enabled',
        startup_warmup: 2,
        startup_raw_ms: startup,
        startup_main_ms: stats(startup.map((x) => x.rust_main_to_two_raf_ms)),
        startup_parent_ms: stats(
          startup.map((x) => x.parent_spawn_to_marker_ms),
        ),
        ipc_warmup: 20,
        ipc_roundtrip_ms: stats(ipc.samples),
        ipc_max_response_bytes: ipc.responseBytes,
        ipc_batch_ms: ipc.batchMs,
        ipc_mean_ms: ipc.batchMs / ipc.samples.length,
        ipc_timer_note:
          'Per-call performance.now is quantized; zero is below timer resolution, not zero latency. Batch mean also includes JavaScript loop overhead.',
        idle: JSON.parse(memoryOutput),
      },
      null,
      2,
    ),
  );
} finally {
  socket?.close();
  for (const waiter of pending.values()) clearTimeout(waiter.timer);
  await stop();
}
