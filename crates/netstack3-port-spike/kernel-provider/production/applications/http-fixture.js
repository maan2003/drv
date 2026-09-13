// Disposable application acceptance fixture. Node's HTTP implementation is the
// application under test; no host networking configuration is changed.
'use strict';
const http = require('node:http');
const fs = require('node:fs');
const crypto = require('node:crypto');
const { WebSocketServer } = require('ws');
const payload = Buffer.alloc(1024 * 1024, 0x5a);
const digest = crypto.createHash('sha256').update(payload).digest('hex');
const page = `<!doctype html><title>Running drv application checks</title>
<h1 id="result">Running</h1><script>
(async () => {
  const fail = m => { throw new Error(m); };
  const bytes = await (await fetch('/redirect')).arrayBuffer();
  const hash = [...new Uint8Array(await crypto.subtle.digest('SHA-256', bytes))]
    .map(x => x.toString(16).padStart(2,'0')).join('');
  if (hash !== '${digest}') fail('download mismatch');
  const upload = await (await fetch('/upload', {method:'POST', body:bytes})).text();
  if (upload !== '${digest}') fail('upload mismatch');
  await Promise.all(Array.from({length:8}, async () => {
    const r = await fetch('/payload');
    if ((await r.arrayBuffer()).byteLength !== 1048576) fail('parallel mismatch');
  }));
  await new Promise((resolve, reject) => {
    const socket = new WebSocket('ws://'+location.host+'/echo');
    socket.binaryType = 'arraybuffer';
    const timer = setTimeout(() => { socket.close(); reject(new Error('WebSocket timeout')); }, 10000);
    socket.onopen = () => socket.send(bytes);
    socket.onerror = () => { clearTimeout(timer); reject(new Error('WebSocket error')); };
    socket.onmessage = async event => {
      const echoHash = [...new Uint8Array(await crypto.subtle.digest('SHA-256', event.data))]
        .map(x => x.toString(16).padStart(2,'0')).join('');
      clearTimeout(timer); socket.close();
      if (echoHash !== '${digest}') reject(new Error('WebSocket mismatch'));
      else resolve();
    };
  });
  document.title = document.querySelector('#result').textContent = 'PASS_BROWSER_HTTP_WEBSOCKET';
  await fetch('/result', {method:'POST', body:'PASS_BROWSER_HTTP_WEBSOCKET'});
})().catch(async e => {
  document.querySelector('#result').textContent = 'FAIL: '+e.message;
  await fetch('/result', {method:'POST', body:'FAIL: '+e.message});
});
</script>`;
const handler = (req, res) => {
  if (req.method === 'GET' && req.url === '/') {
    res.writeHead(200, {'content-type':'text/html'}); res.end(page);
  } else if (req.method === 'GET' && req.url === '/redirect') {
    res.writeHead(302, {location:'/payload'}); res.end();
  } else if (req.method === 'GET' && req.url === '/payload') {
    res.writeHead(200, {'content-type':'application/octet-stream', 'content-length':payload.length});
    res.end(payload);
  } else if (req.method === 'POST' && ['/upload','/result'].includes(req.url)) {
    let count = 0; const hash = crypto.createHash('sha256'); const chunks = [];
    req.on('data', chunk => {
      count += chunk.length;
      if (count > (req.url === '/result' ? 1024 : payload.length)) { req.destroy(); return; }
      if (req.url === '/result') chunks.push(chunk); else hash.update(chunk);
    });
    req.on('end', () => {
      if (req.url === '/result') {
        const result = Buffer.concat(chunks).toString();
        fs.writeFileSync('/run/applications/browser-result', result);
        console.log(result); res.end('recorded');
      } else res.end(hash.digest('hex'));
    });
  } else { res.writeHead(404); res.end(); }
};
const address = process.argv[2] || '127.0.0.1';
const server = http.createServer(handler);
const sockets = new WebSocketServer({server, path:'/echo', maxPayload:payload.length, perMessageDeflate:false});
sockets.on('connection', socket => {
  socket.on('error', () => socket.terminate());
  socket.on('message', (data, binary) => socket.send(data, {binary}));
});
server.requestTimeout = 15000;
server.listen(8080, address, () => console.log('APP_HTTP_READY '+address));
