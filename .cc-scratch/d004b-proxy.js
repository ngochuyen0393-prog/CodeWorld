// D-004b 对比截图代理：5190 -> 转发上游 vite :5180（上游零改动），对 HTML 响应
// 尾部注入「非 async hold script」（推迟 load 事件但不阻塞主线程），让无头截图
// 锚定在 KallkaGo demo 资源加载+KTX2 解码完成后的稳定帧
'use strict';
const http = require('http');

const HOLD_MS = 15000;
const UP_HOST = '127.0.0.1', UP_PORT = 5180, PROXY_PORT = 5190;

http.createServer((req, res) => {
  const p = req.url.split('?')[0];
  if (p === '/__hold') {
    console.log('[proxy] hold', HOLD_MS + 'ms');
    setTimeout(() => { res.writeHead(200, { 'Content-Type': 'text/javascript' }); res.end('/* hold */'); }, HOLD_MS);
    return;
  }
  const up = http.request({ host: UP_HOST, port: UP_PORT, path: req.url, method: req.method, headers: { ...req.headers, host: UP_HOST + ':' + UP_PORT } }, (upRes) => {
    const ct = upRes.headers['content-type'] || '';
    if (ct.includes('text/html')) {
      // HTML：缓冲后注入 hold script（动态 non-async script 推迟 load 事件）
      const chunks = [];
      upRes.on('data', (c) => chunks.push(c));
      upRes.on('end', () => {
        let html = Buffer.concat(chunks).toString('utf8');
        const inject = '<script>var s=document.createElement("script");s.src="/__hold";s.async=false;document.head.appendChild(s);</script>';
        html = html.replace('</body>', inject + '</body>');
        res.writeHead(upRes.statusCode, { ...upRes.headers, 'content-length': Buffer.byteLength(html) });
        res.end(html);
        console.log('[proxy] html 注入 hold ->', req.url);
      });
    } else {
      res.writeHead(upRes.statusCode, upRes.headers);
      upRes.pipe(res);
    }
  });
  up.on('error', (e) => { res.writeHead(502); res.end('proxy error: ' + e.message); });
  req.pipe(up);
}).listen(PROXY_PORT, '127.0.0.1', () => console.log('[proxy] listening on 127.0.0.1:' + PROXY_PORT + ' -> ' + UP_HOST + ':' + UP_PORT));
