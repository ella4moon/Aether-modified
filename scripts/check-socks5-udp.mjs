// Standard-library SOCKS5 control-path diagnostic. No routes or proxy settings
// are changed. A successful UDP association is not a UDP delivery/exit-IP test.
import net from 'node:net';
import { once } from 'node:events';
import { createInterface } from 'node:readline/promises';
import { pathToFileURL } from 'node:url';

const descriptions = [
  'accepted', 'general server failure', 'not allowed by ruleset',
  'network unreachable', 'host unreachable', 'connection refused',
  'TTL expired', 'command not supported', 'address type not supported',
];

export async function probe({ host, port, username, password, command, timeoutMs = 10000 }) {
  const user = Buffer.from(username, 'utf8');
  const pass = Buffer.from(password, 'utf8');
  if (!host || !Number.isInteger(port) || port < 1 || port > 65535) {
    throw new Error('Enter a host and a port between 1 and 65535.');
  }
  if (!user.length || !pass.length || user.length > 255 || pass.length > 255) {
    throw new Error('Username and password must each contain 1–255 UTF-8 bytes.');
  }
  if (command !== 1 && command !== 3) throw new Error('Only CONNECT and UDP ASSOCIATE are tested.');
  const result = { operation: command === 1 ? 'TCP CONNECT example.com:443' : 'UDP ASSOCIATE 0.0.0.0:0' };
  const socket = net.createConnection({ host, port });
  // The async iterator reports errors too; retain a listener across handshakes.
  socket.on('error', () => {});
  const timer = setTimeout(() => socket.destroy(new Error('Handshake timed out')), timeoutMs);
  const chunks = socket[Symbol.asyncIterator]();
  let pending = Buffer.alloc(0);
  async function read(size) {
    while (pending.length < size) {
      const next = await chunks.next();
      if (next.done) throw new Error('Proxy closed the connection before its reply was complete');
      pending = Buffer.concat([pending, next.value]);
    }
    const value = pending.subarray(0, size);
    pending = pending.subarray(size);
    return value;
  }
  let stage = 'TCP connection to proxy';
  try {
    await once(socket, 'connect');
    stage = 'SOCKS5 authentication method';
    socket.write(Buffer.from([5, 1, 2]));
    const method = await read(2);
    if (method[0] !== 5 || method[1] !== 2) throw new Error(`Unexpected method reply: ${method.toString('hex')}`);
    stage = 'Username/password authentication';
    socket.write(Buffer.concat([Buffer.from([1, user.length]), user, Buffer.from([pass.length]), pass]));
    const auth = await read(2);
    if (auth[0] !== 1 || auth[1] !== 0) throw new Error('Proxy rejected authentication');
    result.authentication = 'accepted';
    stage = result.operation;
    const request = command === 3
      ? Buffer.from([5, 3, 0, 1, 0, 0, 0, 0, 0, 0])
      : Buffer.concat([Buffer.from([5, 1, 0, 3, 11]), Buffer.from('example.com'), Buffer.from([1, 187])]);
    result.request_hex = request.toString('hex').match(/../g).join(' ');
    socket.write(request);
    const header = await read(4);
    result.reply_header_hex = header.toString('hex').match(/../g).join(' ');
    if (header[0] !== 5 || header[2] !== 0) throw new Error('Malformed SOCKS5 reply');
    result.reply_code = header[1];
    result.result = descriptions[header[1]] ?? `unknown reply code ${header[1]}`;
    if (header[1] === 0) {
      const length = header[3] === 1 ? 4 : header[3] === 4 ? 16 : header[3] === 3 ? (await read(1))[0] : 0;
      if (!length) throw new Error('Invalid bound address in successful reply');
      const address = await read(length);
      result.bound_address = header[3] === 1 ? [...address].join('.')
        : header[3] === 3 ? address.toString('utf8') : address.toString('hex').match(/.{4}/g).join(':');
      result.bound_port = (await read(2)).readUInt16BE();
    }
  } catch (error) {
    result.error_stage = stage;
    result.error = error.message;
  } finally {
    clearTimeout(timer);
    socket.destroy();
  }
  return result;
}

async function main() {
  const [host, portText] = process.argv.slice(2);
  const port = Number(portText);
  if (!host || !Number.isInteger(port) || port < 1 || port > 65535 || process.argv.length !== 4) {
    console.error('Usage: node check-socks5-udp.mjs PROXY_HOST PROXY_PORT');
    process.exitCode = 2;
    return;
  }
  console.log('SOCKS5 direct diagnostic — no application proxy settings are used.');
  console.log('For a direct comparison, disconnect Aether whole-laptop mode and other VPN/TUN apps first.');
  const input = createInterface({ input: process.stdin, output: process.stdout });
  let username, password;
  try {
    username = (await input.question('Proxy username: ')).trim();
    password = await input.question('Proxy password (visible while typing): ');
  } finally {
    input.close();
  }
  console.log(`\nUTC: ${new Date().toISOString()}\nProxy: ${host}:${port}`);
  for (const command of [1, 3]) {
    console.log(JSON.stringify(await probe({ host, port, username, password, command }), null, 2));
  }
  console.log('UDP accepted = association handshake accepted only; UDP data delivery has not been tested.');
  console.log('Reply 7 = the endpoint rejected the command. A connection error/timeout is inconclusive.');
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  main().catch(error => { console.error(error.message); process.exitCode = 1; });
}
