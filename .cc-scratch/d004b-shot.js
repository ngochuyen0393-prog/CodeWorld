// 无头 Edge 截图包装 v2：--timeout 真实等待模式（KTX2 解码/wasm 编译是真实异步，
// virtual-time-budget 会在解码完成前快进截屏）。
// 用法：node .cc-scratch/d004b-shot.js <输出png> <url> [真实等待毫秒=20000]
'use strict';
const { spawn } = require('child_process');
const fs = require('fs');

const out = process.argv[2];
const url = process.argv[3];
const wait = process.argv[4] || '20000';
if (!out || !url) { console.error('用法: node d004b-shot.js <out.png> <url> [waitMs]'); process.exit(2); }

const EDGE = 'C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe';
const args = [
  '--headless=new', '--disable-gpu', '--window-size=1280,720',
  '--screenshot=' + out, '--timeout=' + wait, url,
];
console.log('[shot]', url, '->', out, '(真实等待 ' + wait + 'ms)');
const t0 = Date.now();
const p = spawn(EDGE, args, { stdio: ['ignore', 'pipe', 'pipe'] });
let errText = '';
p.stderr.on('data', (d) => { errText += d; });
p.on('error', (e) => { console.error('spawn 失败:', e.message); process.exit(3); });
p.on('exit', (code) => {
  const ok = fs.existsSync(out) && fs.statSync(out).size > 1000;
  console.log('[shot] exit=' + code + ' 耗时=' + ((Date.now() - t0) / 1000).toFixed(1) + 's 产物=' + (ok ? fs.statSync(out).size + 'B' : '缺失'));
  if (!ok) console.error('[shot] stderr 尾部:', errText.slice(-800));
  process.exit(ok ? 0 : 1);
});
