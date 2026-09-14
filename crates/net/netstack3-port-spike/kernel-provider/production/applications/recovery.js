// Disposable no-INET recovery gate. Local DoH is a test upstream, never fallback.
'use strict';
const fs = require('node:fs');
const {spawn} = require('node:child_process');
const http = require('node:http');
const http2 = require('node:http2');
const dgram = require('node:dgram');
const {WebSocketServer} = require('ws');
const crypto = require('node:crypto');
const base = '/etc/applications';
const state = '/run/applications/recovery-state';
const sleep = ms => new Promise(r => setTimeout(r, ms));
const assert = (ok, message) => { if (!ok) throw new Error(message); };
const payload = Buffer.alloc(128 * 1024, 0x5a);
const digest = crypto.createHash('sha256').update(payload).digest('hex');

async function fixture(generation) {
  let nextConnection = 0;
  const page = `<!doctype html><title>drv recovery</title><script>
  let closes=0, seq=0, socket, timer, busy=false;
  function connect() {
    socket = new WebSocket('ws://127.0.0.1:8080/echo');
    socket.onopen = () => beat();
    socket.onclose = () => { closes++; clearTimeout(timer); setTimeout(connect, 100); };
    socket.onerror = () => socket.close();
    async function beat() {
      if (socket.readyState !== WebSocket.OPEN) return;
      socket.send(JSON.stringify({seq:++seq,closes}));
      timer=setTimeout(()=>socket.close(), 3000);
    }
    socket.onmessage = async e => {
      clearTimeout(timer);
      if (busy) return; busy=true;
      const reply=JSON.parse(e.data);
      try {
        const bytes=await (await fetch('/payload', {cache:'no-store',signal:AbortSignal.timeout(3000)})).arrayBuffer();
        const hash=[...new Uint8Array(await crypto.subtle.digest('SHA-256',bytes))].map(x=>x.toString(16).padStart(2,'0')).join('');
        if(hash!=='${digest}') throw Error('download integrity');
        const uploaded=await (await fetch('/upload',{method:'POST',body:bytes,signal:AbortSignal.timeout(3000)})).text();
        if(uploaded!==hash) throw Error('upload integrity');
        await fetch('/receipt',{method:'POST',body:JSON.stringify({...reply,bytes:bytes.byteLength}),signal:AbortSignal.timeout(3000)});
      } catch(e) { socket.close(); }
      finally { busy=false; setTimeout(beat, 100); }
    };
  }
  connect();
  </script>`;
  const server = http.createServer((req,res) => {
    if (req.url === '/') return res.end(page);
    if (req.url === '/payload') return res.end(payload);
    if (req.method !== 'POST' || !['/receipt','/upload'].includes(req.url)) {
      res.writeHead(404); return res.end();
    }
    const chunks=[]; let size=0;
    req.on('data', b => { size+=b.length; if(size>payload.length) req.destroy(); else chunks.push(b); });
    req.on('end', () => {
      const body=Buffer.concat(chunks);
      if(req.url==='/upload') res.end(crypto.createHash('sha256').update(body).digest('hex'));
      else { console.log('RECEIPT '+body.toString()); res.end('ok'); }
    });
  });
  const ws = new WebSocketServer({server,path:'/echo',maxPayload:4096,perMessageDeflate:false});
  ws.on('connection', socket => {
    const connection = `${generation}:${++nextConnection}`;
    socket.on('error',()=>socket.terminate());
    socket.on('message', data => {
      const message=JSON.parse(data);
      socket.send(JSON.stringify({...message,connection,generation,nonce:fs.readFileSync(state,'utf8')}));
    });
  });
  // Deliberately narrow, deterministic A/AAAA fixture: validates its generated
  // question format and returns TTL=0 loopback answers. Not a general DNS server.
  const tls = http2.createSecureServer({
    key:fs.readFileSync(base+'/recovery-key.pem'),
    cert:fs.readFileSync(base+'/recovery-cert.pem')
  });
  tls.on('sessionError',()=>{});
  tls.on('stream',(stream, headers) => {
    stream.on('error',()=>{});
    let chunks=[], size=0;
    stream.on('data',b=>{size+=b.length; if(size>512) stream.close(); else chunks.push(b);});
    stream.on('end',()=>{
      const q=Buffer.concat(chunks);
      assert(headers[':path']==='/dns-query' && q.length>=17 && q.readUInt16BE(4)===1,'DoH query');
      let off=12, labels=[];
      while(q[off]) { const n=q[off++]; assert(n<=63 && off+n<q.length,'DoH label'); labels.push(q.subarray(off,off+n).toString()); off+=n; }
      off++;
      const type=q.readUInt16BE(off), end=off+4;
      assert(q.readUInt16BE(off+2)===1 && [1,28].includes(type),'DoH fixture type/class');
      console.log('QUERY '+JSON.stringify({name:labels.join('.'),type,generation}));
      const data=type===1?Buffer.from([127,0,0,1]):Buffer.from('00000000000000000000000000000001','hex');
      const header=Buffer.from(q.subarray(0,12));
      header.writeUInt16BE(0x8180,2); header.writeUInt16BE(1,6);
      header.writeUInt16BE(0,8); header.writeUInt16BE(0,10);
      const rr=Buffer.alloc(12); rr.writeUInt16BE(0xc00c,0); rr.writeUInt16BE(type,2);
      rr.writeUInt16BE(1,4); rr.writeUInt16BE(data.length,10);
      stream.respond({':status':200,'content-type':'application/dns-message'});
      stream.end(Buffer.concat([header,q.subarray(12,end),rr,data]));
    });
  });
  await Promise.all([
    new Promise(r=>server.listen(8080,'127.0.0.1',r)),
    new Promise(r=>tls.listen(8443,'127.0.0.1',r))
  ]);
  console.log('FIXTURE_READY');
}

async function gate() {
  const children = new Set(), receipts=[], queries=[], samples=[];
  let browser, provider, dns, fixtureProcess, kmsg, nonceCounter=0, kernelWarning=false;
  const log = msg => console.log(`${Date.now()} ${msg}`);
  function child(name, exe, args=[], options={}) {
    const p=spawn(exe,args,{stdio:['pipe','pipe','pipe'],...options});
    p.name=name; p.text=''; children.add(p);
    p.done=new Promise(resolve=>p.on('exit',(code,signal)=>{
      p.ended=true; children.delete(p); log(`EXIT ${name} pid=${p.pid} code=${code} signal=${signal}`); resolve({code,signal});
    }));
    p.on('error', e=>{p.error=e;});
    for(const stream of [p.stdout,p.stderr]) {
      let rest='';
      stream.on('data', data=>{
        p.text=(p.text+data).slice(-65536);
        rest+=data;
        for(let end;(end=rest.indexOf('\n'))>=0;) {
          const line=rest.slice(0,end); rest=rest.slice(end+1);
          log(`${name} ${line}`);
          if(name==='kmsg') {
            const priority=/^(\d+),/.exec(line);
            if((priority && Number(priority[1])<=4) ||
              /\b(WARNING:|BUG:|Oops:|kernel BUG|general protection fault|Kernel panic)/i.test(line))
              kernelWarning=true;
          }
          if(line.startsWith('RECEIPT ')) receipts.push(JSON.parse(line.slice(8)));
          if(line.startsWith('QUERY ')) queries.push(JSON.parse(line.slice(6)));
        }
      });
    }
    log(`START ${name} pid=${p.pid}`); return p;
  }
  async function until(label, predicate, ms=10000) {
    const end=Date.now()+ms;
    while(Date.now()<end) { if(predicate()) return; await sleep(25); }
    throw new Error(`deadline ${ms}ms: ${label}`);
  }
  async function ready(p, marker) {
    await until(`${p.name} ${marker}`,()=>{
      if(p.text.includes(marker)) return true;
      assert(!p.error && !p.ended,`${p.name} exited before ${marker}: ${p.text}`);
      return false;
    });
  }
  async function stop(p, signal='SIGKILL') {
    if(!p || p.ended) return;
    if(p.servicePid) process.kill(p.servicePid,signal); else p.kill(signal);
    await until(`${p.name} reaped`,()=>p.ended,5000);
  }
  async function run(name,exe,args=[],options={}) {
    const p=child(name,exe,args,options);
    await until(`${name} completed`,()=>p.ended,15000);
    const result=await p.done; assert(result.code===0,`${name} failed: ${p.text}`); return p.text;
  }
  async function startProvider() {
    const fd=fs.openSync('/dev/netstack3','r+');
    try { provider=child('provider','/bin/netstack3-provider',[],{stdio:['ignore','pipe','pipe',fd]}); }
    finally {fs.closeSync(fd);}
    await ready(provider,'sandbox_ready=true');
  }
  async function startDNS() {
    // The trusted supervisor owns stale pathname removal, strictly after reap.
    const end=Date.now()+10000;
    do {
      fs.rmSync('/run/drv-resolver.sock',{force:true});
      const cfg=fs.openSync(base+'/recovery.toml','r'), ca=fs.openSync(base+'/recovery-ca.pem','r');
      try { dns=child('dns','/bin/sh',['-c',"set -o pipefail; /bin/sh -c 'echo $$ > /run/recovery-dns.pid; exec /bin/drv-dns-service 2>&1' | /bin/cat"],{stdio:['ignore','pipe','pipe',cfg,ca]}); }
      finally {fs.closeSync(cfg); fs.closeSync(ca);}
      await until('DNS startup result',()=>dns.ended||dns.text.includes('DNS_READY'));
      if(!dns.ended && dns.text.includes('DNS_READY')) { dns.servicePid=Number(fs.readFileSync('/run/recovery-dns.pid','utf8')); return; }
      assert(dns.text.includes('Address already in use'),`DNS startup failed: ${dns.text}`);
      log('DNS_BIND_TEARDOWN_RETRY'); await sleep(100);
    } while(Date.now()<end);
    throw Error('DNS teardown did not release ports within 10s');
  }
  async function startFixture(generation) {
    fixtureProcess=child('fixture','/bin/node',[__filename,'fixture',String(generation)],{uid:1000,gid:1000});
    await ready(fixtureProcess,'FIXTURE_READY');
  }
  const phase = label => {
    const nonce=`${++nonceCounter}-${label}`;
    fs.writeFileSync(state,nonce);
    return nonce;
  };
  async function receipt(nonce, previous) {
    let found;
    await until(`browser receipt ${nonce}`,()=>{
      assert(!browser.ended,'persistent Firefox exited');
      found=receipts.find(r=>r.nonce===nonce && (!previous || r.seq>previous.seq));
      return found;
    },15000);
    assert(found.bytes===payload.length,'browser transfer size');
    return found;
  }
  async function wire(name, family) {
    const q=Buffer.concat([Buffer.from('123401000001000000000000','hex'),
      ...name.split('.').map(x=>Buffer.concat([Buffer.from([x.length]),Buffer.from(x)])),
      Buffer.from([0,0,1,0,1])]);
    const socket=dgram.createSocket(family===4?'udp4':'udp6');
    try {
      const answer=await new Promise((resolve,reject)=>{
        const timer=setTimeout(()=>reject(Error('wire DNS timeout')),5000);
        socket.once('error',e=>{clearTimeout(timer);reject(e);});
        socket.once('message',b=>{clearTimeout(timer);resolve(b);});
        socket.send(q,53,family===4?'127.0.0.1':'::1');
      });
      assert(answer.readUInt16BE(0)===0x1234 && (answer.readUInt16BE(2)&15)===0 &&
        answer.readUInt16BE(6)===1 && answer.subarray(-4).equals(Buffer.from([127,0,0,1])),'wire DNS answer');
    } finally {socket.close();}
  }
  async function dnsTraffic(label) {
    const start=queries.length;
    for(const family of [4,6]) await wire(`${label}-wire${family}.recovery.test`,family);
    const name=`${label}-nss.recovery.test`;
    await run('nss','/bin/loopback-test',['resolve',name],{uid:1000,gid:1000});
    const got=queries.slice(start);
    for(const [name,type] of [
      [`${label}-wire4.recovery.test`,1],[`${label}-wire6.recovery.test`,1],
      [`${label}-nss.recovery.test`,1],[`${label}-nss.recovery.test`,28]])
      assert(got.some(q=>q.name===name&&q.type===type),`uncached upstream miss ${name}/${type}`);
  }
  function resources(cycle) {
    assert(kmsg && !kmsg.ended && !kernelWarning,'continuous kernel warning/collector gate');
    const processes=[];
    for(const pid of fs.readdirSync('/proc').filter(x=>/^\d+$/.test(x))) {
      try {
        const status=fs.readFileSync(`/proc/${pid}/status`,'utf8');
        assert(!/State:\s+Z/.test(status),`zombie pid=${pid}`);
        if(!status.includes('VmRSS:')) continue; // exclude kernel threads
        processes.push({pid:Number(pid),name:/Name:\s+(\S+)/.exec(status)[1],
          zombie:/State:\s+Z/.test(status),fds:fs.readdirSync(`/proc/${pid}/fd`).length,
          rss:Number(/VmRSS:\s+(\d+)/.exec(status)[1])});
      } catch(e) {if(e.code!=='ENOENT') throw e;}
    }
    const mem=fs.readFileSync('/proc/meminfo','utf8');
    const sample={cycle,processes:processes.length,fds:processes.reduce((n,p)=>n+p.fds,0),
      rss:processes.reduce((n,p)=>n+p.rss,0),slab:Number(/Slab:\s+(\d+)/.exec(mem)[1]),
      provider:processes.find(p=>p.pid===provider.pid),dns:processes.find(p=>p.pid===dns.servicePid)};
    assert(!processes.some(p=>p.zombie),'no zombies');
    assert(sample.processes<=40 && sample.fds<=2048 && sample.rss<2000000 && sample.slab<512000,'absolute resource limits');
    assert(sample.provider.fds<=32 && sample.dns.fds<=32,'service FD limits');
    samples.push(sample); log('RESOURCE '+JSON.stringify(sample));
    if(cycle>=10) {
      const warm=samples.find(s=>s.cycle===10);
      assert(sample.processes<=warm.processes+2 && sample.fds<=warm.fds+32 &&
        sample.rss<=warm.rss+262144 && sample.slab<=warm.slab+65536,'post-warmup accumulation limits');
    }
  }
  try {
    kmsg=child('kmsg','/bin/cat',['/dev/kmsg']);
    await startProvider(); phase('initial'); await startFixture(0); await startDNS();
    browser=child('firefox','/bin/sh',['-c',`. ${base}/firefox-env; export HOME=/home/app MOZ_HEADLESS=1 LIBGL_ALWAYS_SOFTWARE=1 FONTCONFIG_FILE=/etc/fonts/fonts.conf; mkdir -p /home/app/recovery-profile; cp ${base}/firefox-user.js /home/app/recovery-profile/user.js; exec /opt/firefox/firefox --headless --no-remote --profile /home/app/recovery-profile http://127.0.0.1:8080/`],{uid:1000,gid:1000,detached:true});
    let last=await receipt(fs.readFileSync(state,'utf8'));
    for(let cycle=1;cycle<=100;cycle++) {
      log(`CYCLE_BEGIN ${cycle}`);
      const observer=child('observer','/bin/loopback-test',['recovery']);
      await ready(observer,'OLD_READY');
      await dnsTraffic(`c${cycle}-before`);
      const before=await receipt(phase(`c${cycle}-before`),last);
      const providerPid=provider.pid, fixturePid=fixtureProcess.pid;
      await stop(dns);
      const during=await receipt(phase(`c${cycle}-dns-absent`),before);
      assert(during.connection===before.connection && during.closes===before.closes,'same WS survives DNS absence');
      await startDNS(); await dnsTraffic(`c${cycle}-dns-replaced`);
      const after=await receipt(phase(`c${cycle}-dns-replaced`),during);
      assert(after.connection===before.connection && after.closes===before.closes &&
        provider.pid===providerPid && fixtureProcess.pid===fixturePid,'DNS-only same connection continuity');
      observer.stdin.write('continuity\n'); await ready(observer,'CONTINUITY_OK');
      log(`DNS_ONLY_PASS ${cycle} connection=${after.connection}`);
      observer.stdin.write('arm\n'); await ready(observer,'BLOCKERS_ARMED');
      // Active browser payloads and observer traffic precede provider-only death.
      await receipt(phase(`c${cycle}-armed`),after);
      await stop(provider);
      observer.stdin.write('dead\n'); await ready(observer,'PROVIDER_DEAD_OBSERVED');
      await until('DNS listener detects provider death',()=>dns.ended,5000);
      log(`PROVIDER_DEAD_BARRIER ${cycle} fixtures_still_alive=${!fixtureProcess.ended}`);
      assert(!fixtureProcess.ended,'fixture must not explain old-socket death');
      await stop(fixtureProcess);
      assert((await fixtureProcess.done).signal==='SIGKILL','fixture must not abort during provider death');
      await startProvider(); await startFixture(cycle); await startDNS();
      await dnsTraffic(`c${cycle}-new-generation`);
      const recovered=await receipt(phase(`c${cycle}-new-generation`),after);
      assert(recovered.generation===cycle && recovered.connection!==after.connection &&
        recovered.closes>after.closes,'persistent browser observed disconnect and reconnected');
      // Fresh TCP/UDP, both families, verified independently of the browser.
      await run('fresh','/bin/loopback-test',['recovery-fresh']);
      observer.stdin.write('replacement\n'); await ready(observer,'OLD_STILL_DEAD');
      await until('observer reaped',()=>observer.ended);
      assert((await observer.done).code===0,'observer success');
      last=recovered; resources(cycle);
      receipts.length=0; queries.length=0;
      log(`CYCLE_PASS ${cycle}`);
    }
    assert(!kmsg.ended && !kernelWarning,'continuous kernel warning/oops gate');
  } finally {
    if(browser&&!browser.ended) {try{process.kill(-browser.pid,'SIGKILL');}catch{}}
    for(const p of [...children]) if(p!==kmsg) await stop(p);
    if(kmsg) {
      assert(!kmsg.ended,'kernel collector survived through cleanup');
      const marker='drv_recovery_checkpoint_'+crypto.randomBytes(16).toString('hex');
      fs.writeFileSync('/dev/kmsg',marker+'\n');
      await ready(kmsg,marker);
      await stop(kmsg);
      assert(!kernelWarning,'kernel collector drained through cleanup checkpoint');
    }
  }
  assert(children.size===0,'all direct children reaped');
  log('PASS RECOVERY_100_LOOPBACK_GENERATIONS');
}
(process.argv[2]==='fixture' ? fixture(Number(process.argv[3])) : gate()).catch(e=>{
  console.error('RECOVERY_FAILED '+e.stack); process.exitCode=1;
});
