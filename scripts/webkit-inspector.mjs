// Minimal WebKit inspector client for real desktop tests. No production hook.
export async function connectInspector(endpoint = 'http://127.0.0.1:9223') {
  const listing = await (await fetch(endpoint)).text();
  const target = listing.match(/\/socket\/\d+\/\d+\/WebPage/);
  if (!target) throw new Error('No inspectable WebKit page');
  let targetId,
    nextId = 1;
  const pending = new Map();
  const socket = new WebSocket(
    `${endpoint.replace('http:', 'ws:')}${target[0]}`,
  );
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
  if (!targetId) throw new Error('No WebKit page target');

  return {
    send,
    evaluate,
    close() {
      socket.close();
      for (const waiter of pending.values()) {
        clearTimeout(waiter.timer);
        waiter.reject(new Error('Inspector closed'));
      }
      pending.clear();
    },
  };
}
