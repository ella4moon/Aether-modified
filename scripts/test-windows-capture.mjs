// CI integration test: real Windows TUN -> bundled sing-box -> loopback SOCKS5.
// Only benchmark-address /32s are routed. No public proxy or credentials.
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
const [configPath, binaryPath, probePath] = process.argv.slice(2);
assert(configPath && binaryPath && probePath, 'Pass the UDP config, sing-box.exe and capture-check.exe');
const config = JSON.parse(await readFile(configPath, 'utf8'));
assert.equal(config.outbounds[0].type, 'socks');
assert.equal(config.outbounds[0].network, undefined, 'Use the generated --udp configuration');
assert.equal(config.inbounds[0].stack, 'gvisor');
assert.equal(config.inbounds[0].strict_route, true);
assert.equal(config.inbounds[0].dns_mode, 'hijack');
const target = '198.18.0.42';
const adapter = `AetherCaptureTest-${process.pid}`;
const directory = await mkdtemp(path.join(tmpdir(), 'aether-capture-'));
const clients = new Set();
const errors = [];
const logs = [];
let tcpSeen = false, udpSeen = false, dnsSeen = false, child, probeChild, tcp, udp;
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
  const port = (await read(2)).readUInt16BE();
  const reply = Buffer.from([5, 0, 0, 1, 127, 0, 0, 1, 0, 0]);
  if (request[1] === 3) {
    reply.writeUInt16BE(udpRelay.address().port, 8);
    socket.write(reply);
    while (!(await chunks.next()).done) { /* Keep association control alive. */ }
  } else {
    assert.equal(request[1], 1);
    if (address === '1.1.1.1' && port === 53) {
      socket.write(reply);
      const query = await read((await read(2)).readUInt16BE());
      let end = 12;
      while (end < query.length && query[end] !== 0) {
        assert(query[end] < 64, 'Uncompressed DNS question expected');
        end += query[end] + 1;
      }
      end += 5; // Root label plus QTYPE/QCLASS.
      assert(end <= query.length);
      const expected = Buffer.from('\x07example\x03com\x00\x00\x01\x00\x01', 'binary');
      const known = query.subarray(12, end).equals(expected);
      const response = Buffer.from(query.subarray(0, end));
      response[2] = 0x81;
      response[3] = known ? 0x80 : 0x83;
      response.writeUInt16BE(known ? 1 : 0, 6);
      response.writeUInt32BE(0, 8); // No authority or additional records.
      const answer = known ? Buffer.from([0xc0, 12, 0, 1, 0, 1, 0, 0, 0, 60, 0, 4, 198, 18, 0, 42]) : Buffer.alloc(0);
      const size = Buffer.alloc(2);
      size.writeUInt16BE(response.length + answer.length);
      if (known) dnsSeen = true;
      socket.end(Buffer.concat([size, response, answer]));
      return;
    }
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
  config.inbounds[0].route_address = [`${target}/32`, '198.18.0.43/32'];
  // Exercise native DNS and strict-route WFP filters too. Windows DNS settings
  // on physical adapters are not edited. The temporary TUN and filters are
  // removed below. Only the mock upstream uses DNS/TCP instead of public DoH.
  config.dns.servers = [{
    type: 'tcp', tag: 'through-final-proxy', server: '1.1.1.1', server_port: 53,
    detour: 'local-final-proxy',
  }];
  const baseFilename = path.join(directory, 'base.json');
  await writeFile(baseFilename, JSON.stringify(config));
  probeChild = spawn(path.resolve(probePath), [baseFilename, '198.18.0.2:53'], {
    windowsHide: true, stdio: ['pipe', 'pipe', 'pipe'],
  });
  const probeLines = createInterface({ input: probeChild.stdout });
  const firstLine = once(probeLines, 'line');
  probeChild.stderr.on('data', chunk => logs.push(chunk.toString()));
  const probeExit = once(probeChild, 'exit').then(([code]) => {
    assert.equal(code, 0, 'Production TCP/DNS packet checks must pass');
  });
  probeExit.catch(() => {});
  const [generatedConfig] = await deadline(Promise.race([
    firstLine, probeExit.then(() => { throw new Error('Probe exited before config generation'); }),
  ]), 10000, 'Generating production capture probe');
  probeLines.on('line', line => console.log(line));
  const filename = path.join(directory, 'capture.json');
  await writeFile(filename, generatedConfig);
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
  probeChild.stdin.end('\n');
  await deadline(probeExit, 25000, 'Production capture and DNS checks');
  assert(dnsSeen, 'A real DNS question must reach the SOCKS outbound');

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
  console.log('PASS: real Windows TUN captured TCP, UDP and DNS with production strict-route filters and userspace stack.');
} catch (error) {
  console.error(logs.join('\n'));
  console.error(errors.join('\n'));
  throw error;
} finally {
  tcp?.destroy();
  if (udp) { try { udp.close(); } catch {} }
  if (probeChild && probeChild.exitCode === null) {
    probeChild.kill();
    await deadline(once(probeChild, 'exit'), 5000, 'Stopping capture probe').catch(() => {});
  }
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
    `$ErrorActionPreference = 'Stop'; Get-NetAdapter -IncludeHidden | Where-Object { $_.Name -eq '${adapter}' } | Disable-NetAdapter -Confirm:$false; Clear-DnsClientCache`,
  ], { windowsHide: true, timeout: 8000 });
  await rm(directory, { recursive: true, force: true });
}
