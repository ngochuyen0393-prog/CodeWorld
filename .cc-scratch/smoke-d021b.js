// D-021b A 轨冒烟机证：起真世界（空世界+WS）→ node WS 客户端发 create_player
// → 断言三件：①快照出现 kind=player 实体（glyph=名字首字符）②player_joined 事件
// ③players/<name>.wat 落盘。再发同名 → create_player_rejected NAME_TAKEN。
'use strict';
const NET = require('net');
const { execFile } = require('child_process');
const fs = require('fs');

const WORLD = 'D:/projects/CodeWorld/target/debug/codeworld.exe';
const DIR = 'C:/Users/Administrator/AppData/Local/hermes/cache/scratch/t_be0de36c/smoke_world';
const PORT = 18099;

function sleep(ms) { return new Promise(r => setTimeout(r, ms)); }

// --- 极简 WS 客户端（node 内置无 ws 库环境用裸 socket 握手+帧编解码，够发文本收文本）---
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
    // 解析完整帧（文本帧 opcode 1；忽略 ping/pong/close）
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

async function main() {
  // 干净世界目录
  fs.rmSync(DIR, { recursive: true, force: true });
  fs.mkdirSync(DIR + '/players', { recursive: true });
  // 真 sqlite 旧档：先跑一次世界让它建 db，关掉后留档（比 stub 真实——首建归档
  // 面对的是"上一局世界的真快照"，不是字符串文件）
  {
    const env0 = { ...process.env,
      CODEWORLD_DB_PATH: DIR + '/codeworld.db',
      CODEWORLD_PLAYERS_DIR: DIR + '/players',
      CODEWORLD_EMPTY_WORLD: '1',
      CODEWORLD_WS_PORT: '0',
      RUST_LOG: 'warn',
    };
    const w0 = execFile(WORLD, [], { cwd: 'D:/projects/CodeWorld', env: env0, encoding: 'buffer' });
    let e0 = '';
    w0.stderr.on('data', (d) => { e0 += d.toString(); });
    for (let i = 0; i < 40; i++) { await sleep(250); if (fs.existsSync(DIR + '/codeworld.db')) break; }
    await sleep(500);
    w0.kill();
    if (!fs.existsSync(DIR + '/codeworld.db')) throw new Error('旧档预备失败：' + e0.slice(0, 300));
  }
  console.log('[0] 旧档预备 ✓（真 sqlite codeworld.db）');

  const env = { ...process.env,
    CODEWORLD_DB_PATH: DIR + '/codeworld.db',
    CODEWORLD_PLAYERS_DIR: DIR + '/players', // 绝对路径：CWD=项目根（模板相对解析同 start.bat 生产形态），玩家目录隔离在 scratch
    CODEWORLD_EMPTY_WORLD: '1',
    CODEWORLD_WS_PORT: String(PORT),
    RUST_LOG: 'info',
  };
  const world = execFile(WORLD, [], { cwd: 'D:/projects/CodeWorld', env, encoding: 'buffer' });
  let stderr = '';
  world.stderr.on('data', (d) => { stderr += d.toString(); });

  try {
    // 等世界起来（log 出现空世界行）
    let up = false;
    for (let i = 0; i < 40 && !up; i++) { await sleep(250); up = stderr.includes('空世界模式'); }
    if (!up) throw new Error('世界未起：' + stderr.slice(0, 500));
    console.log('[1] 空世界已起（无开局生物）');

    const ws = await MiniWs.connect(PORT);
    console.log('[2] WS 已连接（第一连接=操控端）');
    // 首帧 avatar_assets → 之后快照帧
    let frame = JSON.parse(await ws.next(5000));
    if (frame.type === 'avatar_assets') frame = JSON.parse(await ws.next(5000));
    if (!Array.isArray(frame.entities)) throw new Error('首帧非快照: ' + JSON.stringify(frame).slice(0, 200));
    console.log('[3] 快照流已收（tick=' + frame.tick + ' entities=' + frame.entities.length + '——应为 0 或不含 player）');
    const hadPlayerBefore = frame.entities.some(e => e.kind === 'player');
    if (hadPlayerBefore) throw new Error('空世界快照不应有 player 实体');

    // 发 create_player
    ws.sendText(JSON.stringify({ type: 'create_player', name: 'tudou', template: 'hunter' }));
    console.log('[4] create_player 已发送（name=tudou template=hunter）');

    // 等回执：扫后续快照帧找 player 实体 + player_joined 事件
    let joined = false, playerEnt = null, rejected = [];
    const deadline = Date.now() + 8000;
    while (Date.now() < deadline && !(joined && playerEnt)) {
      const snap = JSON.parse(await ws.next(5000));
      if (Array.isArray(snap.player_events)) {
        for (const ev of snap.player_events) {
          if (ev.kind === 'player_joined' && ev.name === 'tudou') joined = true;
          if (ev.kind === 'create_player_rejected') rejected.push(ev.reason);
        }
      }
      playerEnt = (snap.entities || []).find(e => e.kind === 'player' && e.name === 'tudou') || playerEnt;
    }
    if (!joined) throw new Error('未收到 player_joined（rejected=' + JSON.stringify(rejected) + '）');
    if (!playerEnt) throw new Error('快照未见 tudou 实体');
    console.log('[5] 出生时刻：player_joined 事件 ✓ 实体进场 ✓ glyph=' + playerEnt.glyph + ' color=' + playerEnt.color + ' pos=(' + playerEnt.x + ',' + playerEnt.y + ') controlled=' + playerEnt.controlled);

    // 文件三件
    const wat = DIR + '/players/tudou.wat';
    const me = DIR + '/players/me.txt';
    if (!fs.existsSync(wat)) throw new Error('players/tudou.wat 未落盘');
    const watSrc = fs.readFileSync(wat, 'utf8');
    if (!watSrc.includes('小猎手')) throw new Error('wat 内容非 hunter 模板');
    if (!fs.existsSync(me) || fs.readFileSync(me, 'utf8').trim() !== 'tudou') throw new Error('me.txt 未接管');
    // 首建归档：阶段0 的旧档已在启动时挪 archive/（空世界+空玩家目录=首启归档）；
    // 本局 db 是全新建的
    if (!fs.existsSync(DIR + '/archive')) throw new Error('archive/ 不存在（首启归档未生效）');
    const archFiles = fs.readdirSync(DIR + '/archive').filter(f => f.includes('codeworld.db'));
    if (!archFiles.length) throw new Error('archive/ 无 db 归档文件');
    console.log('[6] 文件三件 ✓ players/tudou.wat（hunter 模板全文）· me.txt=tudou · 首启归档 ✓ archive/' + archFiles[0]);

    // 同名再建 → NAME_TAKEN
    ws.sendText(JSON.stringify({ type: 'create_player', name: 'tudou', template: 'blank' }));
    let took = false;
    const dl2 = Date.now() + 6000;
    while (Date.now() < dl2 && !took) {
      const snap = JSON.parse(await ws.next(5000));
      for (const ev of (snap.player_events || [])) {
        if (ev.kind === 'create_player_rejected' && ev.reason === 'NAME_TAKEN') took = true;
      }
    }
    if (!took) throw new Error('同名重建未回 NAME_TAKEN');
    console.log('[7] 同名重建 → create_player_rejected NAME_TAKEN ✓');

    console.log('SMOKE OK：A 轨机证全链通过');
    process.exitCode = 0;
  } catch (e) {
    console.log('SMOKE FAIL:', e.message);
    console.log('--- battle.log tail ---');
    console.log(stderr.split('\n').slice(-15).join('\n'));
    process.exitCode = 1;
  } finally {
    world.kill();
  }
}
main();
