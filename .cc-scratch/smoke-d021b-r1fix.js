// R1 修复（改法 B+A）A 轨机证：真世界 + 裸 WS 客户端，专打 R1 缺口场景——
// 前端旧规则曾放行、后端拒绝的名字（乔丹·科比/１２３/😂大王），断言：
// ①解析层放行（消息不再被静默丢弃）②tick 侧回 create_player_rejected
// NAME_BAD_CHAR 事件回执（busy 锁有解锁信号）③合法名照常出生。
// 复用 smoke-d021b.js 的 MiniWs 与起服套路（世界目录独立，不碰原 scratch）。
'use strict';
const NET = require('net');
const { execFile } = require('child_process');
const fs = require('fs');

const WORLD = 'D:/projects/CodeWorld/target/debug/codeworld.exe';
const DIR = 'C:/Users/Administrator/AppData/Local/hermes/cache/scratch/t_be0de36c/smoke_r1';
const PORT = 18123;

function sleep(ms) { return new Promise(r => setTimeout(r, ms)); }

class MiniWs {
  constructor(sock) { this.sock = sock; this.buf = Buffer.alloc(0); this.queue = []; this.waiters = []; }
  static async connect(port) {
    return new Promise((resolve, reject) => {
      const sock = NET.connect(port, '127.0.0.1', () => {
        const key = Buffer.from(Math.random().toString(36).slice(2)).toString('base64');
        sock.write('GET /ws HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: ' + key + '\r\nSec-WebSocket-Version: 13\r\n\r\n');
      });
      sock.on('error', reject);
      let hs = Buffer.alloc(0);
      const onHandshake = (chunk) => {
        hs = Buffer.concat([hs, chunk]);
        const idx = hs.indexOf('\r\n\r\n');
        if (idx < 0) return;
        sock.off('data', onHandshake);
        if (!hs.slice(0, idx).toString().includes('101')) { reject(new Error('handshake fail')); return; }
        const ws = new MiniWs(sock);
        ws.buf = hs.slice(idx + 4);
        sock.on('data', (c) => ws._onData(c));
        resolve(ws);
      };
      sock.on('data', onHandshake);
    });
  }
  _onData(chunk) {
    this.buf = Buffer.concat([this.buf, chunk]);
    while (true) {
      if (this.buf.length < 2) break;
      const b0 = this.buf[0], b1 = this.buf[1];
      const opcode = b0 & 0x0f;
      const masked = (b1 & 0x80) !== 0;
      let len = b1 & 0x7f, off = 2;
      if (len === 126) { if (this.buf.length < 4) break; len = this.buf.readUInt16BE(2); off = 4; }
      else if (len === 127) { if (this.buf.length < 10) break; len = Number(this.buf.readBigUInt64BE(2)); off = 10; }
      let mask = null;
      if (masked) { if (this.buf.length < off + 4) break; mask = this.buf.slice(off, off + 4); off += 4; }
      if (this.buf.length < off + len) break;
      let payload = this.buf.slice(off, off + len);
      if (mask) payload = Buffer.from(payload.map((b, i) => b ^ mask[i % 4]));
      this.buf = this.buf.slice(off + len);
      if (opcode === 1) { this.queue.push(payload.toString('utf8')); this._drain(); }
      if (opcode === 8) { this.sock.end(); }
    }
  }
  _drain() { while (this.queue.length && this.waiters.length) this.waiters.shift()(this.queue.shift()); }
  sendText(text) {
    const p = Buffer.from(text, 'utf8');
    const mask = Buffer.from([1, 2, 3, 4]);
    let head;
    if (p.length < 126) { head = Buffer.alloc(2); head[1] = 0x80 | p.length; }
    else { head = Buffer.alloc(4); head[1] = 0x80 | 126; head.writeUInt16BE(p.length, 2); }
    head[0] = 0x81;
    this.sock.write(Buffer.concat([head, mask, Buffer.from(p.map((b, i) => b ^ mask[i % 4]))]));
  }
  next(timeoutMs) {
    if (this.queue.length) return Promise.resolve(this.queue.shift());
    return new Promise((resolve, reject) => {
      const t = setTimeout(() => reject(new Error('ws frame timeout')), timeoutMs);
      this.waiters.push((m) => { clearTimeout(t); resolve(m); });
    });
  }
}

// 收集未来 deadline 内所有快照的 player_events（按 kind/reason 过滤可传入）
async function collectEvents(ws, deadlineMs, match) {
  const hits = [];
  const deadline = Date.now() + deadlineMs;
  while (Date.now() < deadline) {
    let snap;
    try { snap = JSON.parse(await ws.next(deadline - Date.now() + 100)); }
    catch (e) { break; } // 超时收尾
    for (const ev of (snap.player_events || [])) if (match(ev)) hits.push(ev);
  }
  return hits;
}

async function main() {
  fs.rmSync(DIR, { recursive: true, force: true });
  fs.mkdirSync(DIR + '/players', { recursive: true });
  const env = { ...process.env,
    CODEWORLD_DB_PATH: DIR + '/codeworld.db',
    CODEWORLD_PLAYERS_DIR: DIR + '/players',
    CODEWORLD_EMPTY_WORLD: '1',
    CODEWORLD_WS_PORT: String(PORT),
    RUST_LOG: 'info',
  };
  const world = execFile(WORLD, [], { cwd: 'D:/projects/CodeWorld', env, encoding: 'buffer' });
  let stderr = '';
  world.stderr.on('data', (d) => { stderr += d.toString(); });

  try {
    let up = false;
    for (let i = 0; i < 40 && !up; i++) { await sleep(250); up = stderr.includes('空世界模式'); }
    if (!up) throw new Error('世界未起：' + stderr.slice(0, 400));
    console.log('[1] 空世界已起');

    const ws = await MiniWs.connect(PORT);
    let frame = JSON.parse(await ws.next(5000));
    if (frame.type === 'avatar_assets') frame = JSON.parse(await ws.next(5000));
    console.log('[2] 快照流已收（tick=' + frame.tick + '）');

    // ---- R1 缺口三连：旧前端放行 + 旧后端解析层拒绝 = 静默丢弃 → busy 挂死。
    // 现在的期望：每发都收到 create_player_rejected NAME_BAD_CHAR 事件回执
    const gapNames = ['乔丹·科比', '１２３', '😂大王'];
    for (const name of gapNames) {
      ws.sendText(JSON.stringify({ type: 'create_player', name, template: 'blank' }));
      const hits = await collectEvents(ws, 5000, ev => ev.kind === 'create_player_rejected');
      const bad = hits.find(ev => ev.reason === 'NAME_BAD_CHAR');
      if (!bad) throw new Error(`名字 ${name} 未收到 NAME_BAD_CHAR 回执（hits=${JSON.stringify(hits)}——busy 会挂死）`);
      console.log(`[3] ${JSON.stringify(name)} → NAME_BAD_CHAR 事件回执 ✓（面板会解锁红字重试）`);
      if (fs.readdirSync(DIR + '/players').some(f => f.endsWith('.wat'))) {
        throw new Error('非法名字不应落任何 .wat 文件');
      }
    }
    console.log('[4] 三缺口名全部结构化回执 ✓ 零文件落盘 ✓');

    // ---- 合法中文名照常出生（回归锚：改法 B 不伤主链）
    ws.sendText(JSON.stringify({ type: 'create_player', name: '土豆', template: 'grazer' }));
    let joined = false, ent = null;
    const dl = Date.now() + 8000;
    while (Date.now() < dl && !joined) {
      const snap = JSON.parse(await ws.next(5000));
      for (const ev of (snap.player_events || [])) if (ev.kind === 'player_joined' && ev.name === '土豆') joined = true;
      ent = (snap.entities || []).find(e => e.kind === 'player' && e.name === '土豆') || ent;
    }
    if (!joined || !ent) throw new Error('合法中文名未出生（joined=' + joined + '）');
    if (!fs.existsSync(DIR + '/players/土豆.wat')) throw new Error('players/土豆.wat 未落盘');
    console.log('[5] 合法中文名「土豆」出生 ✓（glyph=' + ent.glyph + '，grazer 模板落盘）');

    // ---- 静默丢弃对照锚：纯垃圾消息（缺字段）仍走解析层丢弃（协议形状归解析层）
    ws.sendText('{"type":"create_player","name":"ok"}'); // 缺 template
    const silent = await collectEvents(ws, 4000, ev => ev.kind === 'create_player_rejected');
    if (silent.length) throw new Error('缺字段消息应解析层静默丢弃，不该有业务事件：' + JSON.stringify(silent));
    console.log('[6] 协议形状错误（缺字段）仍解析层丢弃 ✓（与名字校验分家清晰）');

    console.log('SMOKE OK：R1 修复机证全过（缺口名结构化回执 + 合法名主链无回归）');
    process.exitCode = 0;
  } catch (e) {
    console.log('SMOKE FAIL:', e.message);
    console.log('--- stderr tail ---');
    console.log(stderr.split('\n').slice(-12).join('\n'));
    process.exitCode = 1;
  } finally {
    world.kill();
  }
}
main();
