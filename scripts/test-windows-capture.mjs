// CI integration test: real Windows TUN -> bundled sing-box -> loopback SOCKS5.
// Only one benchmark-address /32 is routed. No public proxy or credentials.
import assert from 'node:assert/strict';
import net from 'node:net';
import dgram from 'node:dgram';
import { once } from 'node:events';
import { spawn, execFileSync } from 'node:child_process';
import { readFile, writeFile, mkdtemp, rm } from 'node:fs/promises';
import { createInterface } from 'node:readline';
import { tmpdir } from 'node:os';
import path from 'node:path';

assert.equal(process.platform, 'win32', 'This test requires an elevated Windows runner');
const [configPath, binaryPath] = process.argv.slice(2);
assert(configPath && binaryPath, 'Pass the generated UDP config and bundled sing-box.exe');
const config = JSON.parse(await readFile(configPath, 'utf8'));
assert.equal(config.outbounds[0].type, 'socks');
assert.equal(config.outbounds[0].network, undefined, 'Use the generated --udp configuration');
const target = '198.18.0.42';
const adapter = `AetherCaptureTest-${process.pid}`;
const directory = await mkdtemp(path.join(tmpdir(), 'aether-capture-'));
const clients = new Set();
const errors = [];
const logs = [];
let tcpSeen = false, udpSeen = false, child, tcp, udp;
const udpRelay = dgram.createSocket('udp4');
udpRelay.on('error', error => errors.push(error.message));
const proxy = net.createServer(socket => {
  clients.add(socket);
  socket.on('close', () => clients.delete(socket));
  socket.on('error', () => {});
  socket.setTimeout(15000, () => socket.destroy());
  handle(socket).catch(error => {
    if (error.code !== 'ECONNRESET') errors.push(error.message);
    socket.destroy();
  });
});

function deadline(promise, ms, operation) {
  let timer;
  return Promise.race([promise, new Promise((_, reject) => {
    timer = setTimeout(() => reject(new Error(`${operation} timed out`)), ms);
  })]).finally(() => clearTimeout(timer));
}

async function handle(socket) {
  const chunks = socket[Symbol.asyncIterator]();
  let pending = Buffer.alloc(0);
  async function read(size) {
    while (pending.length < size) {
      const next = await chunks.next();
      assert(!next.done, 'SOCKS client closed an incomplete request');
      pending = Buffer.concat([pending, next.value]);
    }
    const value = pending.subarray(0, size);
    pending = pending.subarray(size);
    return value;
  }
  const greeting = await read(2);
  assert.equal(greeting[0], 5);
  assert((await read(greeting[1])).includes(0), 'Loopback SOCKS uses no authentication');
  socket.write(Buffer.from([5, 0]));
  const request = await read(4);
  assert.equal(request[0], 5);
  assert.equal(request[2], 0);
  let address;
  if (request[3] === 1) address = [...await read(4)].join('.');
  else if (request[3] === 4) address = (await read(16)).toString('hex');
  else if (request[3] === 3) address = (await read((await read(1))[0])).toString('utf8');
  else throw new Error(`Unexpected SOCKS address type ${request[3]}`);
  await read(2);
  const reply = Buffer.from([5, 0, 0, 1, 127, 0, 0, 1, 0, 0]);
  if (request[1] === 3) {
    reply.writeUInt16BE(udpRelay.address().port, 8);
    socket.write(reply);
    while (!(await chunks.next()).done) { /* Keep association control alive. */ }
  } else {
    assert.equal(request[1], 1);
    assert.equal(address, target, 'TCP must reach the SOCKS outbound through the TUN');
    tcpSeen = true;
    socket.write(reply);
    if (pending.length) socket.write(pending);
    for (let next; !(next = await chunks.next()).done;) socket.write(next.value);
  }
}

udpRelay.on('message', (packet, peer) => {
  try {
    assert.deepEqual([...packet.subarray(0, 4)], [0, 0, 0, 1]);
    assert.equal([...packet.subarray(4, 8)].join('.'), target);
    assert(packet.length > 10);
    udpSeen = true;
    udpRelay.send(packet, peer.port, peer.address);
  } catch (error) { errors.push(error.message); }
});

try {
  udpRelay.bind(0, '127.0.0.1');
  await once(udpRelay, 'listening');
  proxy.listen(0, '127.0.0.1');
  await once(proxy, 'listening');
  config.outbounds[0].server_port = proxy.address().port;
  config.inbounds[0].interface_name = adapter;
  config.inbounds[0].address = ['198.18.0.1/30'];
  config.inbounds[0].route_address = [`${target}/32`];
  // Leave the runner's default routes and DNS/WFP policy alone. The same TUN
  // stack, auto-route code, loopback SOCKS outbound and interface binding run.
  config.inbounds[0].dns_mode = 'disabled';
  config.inbounds[0].strict_route = false;
  const filename = path.join(directory, 'capture.json');
  await writeFile(filename, JSON.stringify(config));
  let started;
  const ready = new Promise(resolve => { started = resolve; });
  child = spawn(path.resolve(binaryPath), ['run', '-c', filename], {
    windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'],
  });
  for (const stream of [child.stdout, child.stderr]) {
    createInterface({ input: stream }).on('line', line => {
      logs.push(line);
      if (logs.length > 60) logs.shift();
      if (line.includes('sing-box started')) started();
    });
  }
  const exited = once(child, 'exit').then(([code]) => { throw new Error(`sing-box exited: ${code}`); });
  // Attach a handler for an exit after startup as well as the startup race.
  exited.catch(() => {});
  await deadline(Promise.race([ready, exited]), 20000, 'TUN startup');

  const payload = Buffer.from(`aether-capture-${process.pid}`);
  tcp = net.createConnection({ host: target, port: 28445 });
  tcp.on('error', () => {});
  await deadline(once(tcp, 'connect'), 8000, 'TCP through TUN');
  const received = (async () => {
    const parts = [];
    let size = 0;
    for await (const part of tcp) {
      parts.push(part);
      size += part.length;
      if (size >= payload.length) return Buffer.concat(parts);
    }
    throw new Error('TCP closed before the echo completed');
  })();
  tcp.write(payload);
  assert.deepEqual(await deadline(received, 8000, 'TCP echo'), payload);
  assert(tcpSeen, 'The local SOCKS relay must observe TCP');
  tcp.destroy();

  udp = dgram.createSocket('udp4');
  udp.on('error', () => {});
  const packet = once(udp, 'message');
  udp.send(payload, 29446, target);
  assert.deepEqual((await deadline(packet, 8000, 'UDP through TUN'))[0], payload);
  assert(udpSeen, 'The local SOCKS relay must observe UDP ASSOCIATE and a datagram');
  assert.deepEqual(errors, []);
  console.log('PASS: real Windows TUN captured ordinary TCP and UDP through the loopback SOCKS5 outbound.');
} catch (error) {
  console.error(logs.join('\n'));
  console.error(errors.join('\n'));
  throw error;
} finally {
  tcp?.destroy();
  if (udp) { try { udp.close(); } catch {} }
  if (child && child.exitCode === null) {
    child.kill();
    await deadline(once(child, 'exit'), 5000, 'Stopping test TUN').catch(() => {});
  }
  for (const socket of clients) socket.destroy();
  if (proxy.listening) await new Promise(resolve => proxy.close(resolve));
  try { udpRelay.close(); } catch {}
  // A forcibly stopped driver can retain its device. Disable only this test's
  // unique adapter. No production adapter or physical interface is touched.
  execFileSync(path.join(process.env.SystemRoot, 'System32/WindowsPowerShell/v1.0/powershell.exe'), [
    '-NoLogo', '-NoProfile', '-NonInteractive', '-Command',
    `$ErrorActionPreference = 'Stop'; Get-NetAdapter -IncludeHidden | Where-Object { $_.Name -eq '${adapter}' } | Disable-NetAdapter -Confirm:$false`,
  ], { windowsHide: true, timeout: 8000 });
  await rm(directory, { recursive: true, force: true });
}
