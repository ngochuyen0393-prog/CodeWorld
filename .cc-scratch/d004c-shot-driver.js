// D-004c 截图驱动：顺序跑 8 张开/关对照截图（msedge headless，任务书规定链路）
'use strict';
const { spawnSync } = require('child_process');
const EDGE = 'C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe';
const BASE = 'http://127.0.0.1:8797/';
const OUT = 'D:/projects/CodeWorld/.cc-scratch/d004c-shots/';

// [文件名, hash]——a/r 开关参数直接进 hash（页面 hashFlag 解析）
const SHOTS = [
  ['d004c-aniso-off-front.png', '#d3d-still-full&a=0'],
  ['d004c-aniso-on-front.png', '#d3d-still-full&a=1'],
  ['d004c-aniso-off-l45.png', '#d3d-still-l45-full&a=0'],
  ['d004c-aniso-on-l45.png', '#d3d-still-l45-full&a=1'],
  ['d004c-rim-off-front.png', '#d3d-still-full&r=0'],
  ['d004c-rim-on-front.png', '#d3d-still-full&r=1'],
  ['d004c-rim-off-l45.png', '#d3d-still-l45-full&r=0'],
  ['d004c-rim-on-l45.png', '#d3d-still-l45-full&r=1'],
];

for (const [file, hash] of SHOTS) {
  const url = BASE + hash;
  console.log('SHOT', file, '<-', hash);
  const r = spawnSync(EDGE, [
    '--headless=new', '--disable-gpu', '--window-size=1280,720',
    '--screenshot=' + OUT + file, '--timeout=30000', url,
  ], { stdio: 'ignore', timeout: 60000 });
  if (r.error) { console.error('  spawn 失败:', r.error.message); process.exit(1); }
  console.log('  exit', r.status);
}
console.log('ALL DONE');
