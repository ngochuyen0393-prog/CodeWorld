// headless 验证用：连上 WS 后立刻下发一次点击走位意图（点向阻挡者坐标）
// 一次连接 = 操控连接（单连接假设）；发送后保持连接 2 秒再退（避免秒断）
const ws = new WebSocket('ws://127.0.0.1:8080/ws');
ws.onopen = () => {
  console.log('WS connected, sending move intent');
  ws.send(JSON.stringify({ type: 'move', x: 30.0, y: 0.5 }));
  setTimeout(() => { ws.close(); process.exit(0); }, 2000);
};
ws.onerror = (e) => { console.error('WS error', e.message || e); process.exit(1); };
setTimeout(() => { console.error('WS connect timeout'); process.exit(1); }, 8000);
