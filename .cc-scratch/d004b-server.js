// D-004b 临时静态服务（截图专用，非长跑服务）：静态文件 + /__hold/N 截图时序锚
'use strict';
const http = require('http'), fs = require('fs'), path = require('path');
const root = 'D:/projects/CodeWorld/bin/codeworld/static';
const MIME = { '.html': 'text/html', '.js': 'text/javascript', '.mjs': 'text/javascript', '.json': 'application/json', '.png': 'image/png', '.glb': 'model/gltf-binary', '.wasm': 'application/wasm', '.css': 'text/css' };
http.createServer((req, res) => {
  let p = decodeURIComponent(req.url.split('?')[0].split('#')[0]);
  // /__log：页面加载链路上报（无头截图看不到 console，卡死时靠这个定位）
  if (p.startsWith('/__log')) {
    const m = req.url.split('m=')[1] ? decodeURIComponent(req.url.split('m=')[1]) : '';
    console.log('[cw3d]', m);
    res.writeHead(200); res.end('ok');
    return;
  }
  // /__hold/N：保持 N 毫秒才响应（推迟 load 事件，等 KTX2 解码+首帧渲染完）
  const hold = p.match(/^\/__hold\/(\d+)$/);
  if (hold) {
    const ms = Math.min(parseInt(hold[1], 10) || 0, 60000);
    console.log('hold', ms + 'ms');
    setTimeout(() => { res.writeHead(200, { 'Content-Type': 'text/javascript' }); res.end('/* hold */'); }, ms);
    return;
  }
  if (p === '/') p = '/index.html';
  const f = path.join(root, p);
  fs.readFile(f, (err, data) => {
    if (err) { res.writeHead(404); res.end('404 ' + p); console.log('404', p); return; }
    res.writeHead(200, { 'Content-Type': MIME[path.extname(f)] || 'application/octet-stream' });
    res.end(data);
    console.log('200', p);
  });
}).listen(8791, '127.0.0.1', () => console.log('serving on http://127.0.0.1:8791'));
