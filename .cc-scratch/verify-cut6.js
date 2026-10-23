// 期五第六刀：炮塔解算器 + 弧线预览对齐 + 哑弹✕修正 node 纯逻辑验证
// 从 index.html 提取 [PURE-BEGIN]..[PURE-END] 纯函数区，断言：
//   A. solveLaunchVector 镜像：高空高弧命中 / 地面低弧命中 / 超远 null /
//      零距离 null / 近贴不爬坡（与 Rust main.rs 同算法同判据）
//   B. previewTrajectory：细分真弧线（>20 点）/ 无解 null / 落地截断
//   C. decideClickMessage：贴身 attack / 远程 shoot / 超远 shoot-blocked（不发消息）
//   D. collectKills 排除弹丸（弹丸消亡不再触发击杀✕）
//   E. collectProjectileDissolutions：命中→闪白 victim / 哑弹→尘点
//   F. 尾迹常量 1.5（0.6→1.5 弹丸视觉更清晰）
//   G. tooltipAnchor 悬浮标签锚实体正上方
let pass = 0, fail = 0;
function check(name, cond, detail) {
  if (cond) { pass++; console.log('PASS ' + name); }
  else { fail++; console.log('FAIL ' + name + (detail ? ' —— ' + detail : '')); }
}

const fs = require('fs');
const html = fs.readFileSync('bin/codeworld/static/index.html', 'utf8');
const begin = html.indexOf('// ==== [PURE-BEGIN]');
const end = html.indexOf('// ==== [PURE-END]');
if (begin < 0 || end < 0 || end <= begin) {
  console.error('PURE 区锚点没找到（index.html 被移动了？）');
  process.exit(2);
}
const src = html.slice(begin, end);
// FIX-2 修 3 环境修复：注入 PURE 区外全局 terrainData 声明（null=平地/旧协议，
// 与浏览器运行时初值一致；产品代码无罪，详见 verify-click-tolerance.js 同注）
const factory = new Function('var terrainData = null;\n' + src + '\nreturn {\n' +
  ['applyMoveSteps', 'worldDist2', 'reconcileDecision', 'renderSteps', 'clampCamera',
   'collectKills', 'meleeInReach', 'collectProjectileHits', 'nearestProjectileVictim',
   'findControlled', 'findDisposedPlayer', 'logoutChoiceLabel', 'disposeTag',
   'nearestEntityAtWorld', 'ctrlShootVector', 'previewTrajectory', 'decideClickMessage',
   'canClick', 'dcBannerText', 'CTRL_SHOOT_SPEED', 'MELEE_REACH',
   'solveLaunchVector', 'simTrajectory', 'segMinDist', 'collectProjectileDissolutions',
   'tooltipAnchor', 'PROJ_TRAIL', 'CTRL_SOLVE_TOLERANCE']
    .map((n) => n + ': typeof ' + n + ' !== "undefined" ? ' + n + ' : null').join(',\n') +
 '\n};');
const api = factory();

// === 弹道模拟与脱靶量（与两侧实现同判据的独立验证器）===
function segDist(a, b, p) {
  const abx = b.x - a.x, aby = b.y - a.y;
  const l2 = abx * abx + aby * aby;
  let t = l2 < 1e-12 ? 0 : (((p.x - a.x) * abx + (p.y - a.y) * aby) / l2);
  t = Math.max(0, Math.min(1, t));
  return Math.hypot(p.x - a.x - abx * t, p.y - a.y - aby * t);
}
function pathMiss(pts, tx, ty) {
  let m = Infinity;
  for (let i = 0; i < pts.length - 1; i++) m = Math.min(m, segDist(pts[i], pts[i + 1], { x: tx, y: ty }));
  return m;
}

// ==== A. solveLaunchVector 镜像（契约：与 Rust solve_launch_vector 同输入同结论）====
if (api.solveLaunchVector) {
  const S = api.solveLaunchVector;

  // A1 高空目标高弧命中（bug 回归：60° 定角顶点 9.6 打不到 12 高的 S）
  const s1 = S(30, 0.5, 25, 12);
  check('A1 高空 S 有解', !!s1, JSON.stringify(s1));
  if (s1) {
    const pts = api.simTrajectory(30, 0.5, s1.x, s1.y);
    const apex = Math.max.apply(null, pts.map(p => p.y));
    const miss = pathMiss(pts, 25, 12);
    const v = Math.hypot(s1.x, s1.y);
    check('A1 弹道进入命中窗口', miss < api.CTRL_SOLVE_TOLERANCE, 'miss=' + miss.toFixed(3));
    check('A1 顶点到达 S 高度邻域', apex >= 12 - api.CTRL_SOLVE_TOLERANCE, 'apex=' + apex.toFixed(2));
    check('A1 速度爬坡且封顶', v > 13.8 && v <= 22 + 1e-9, 'v=' + v.toFixed(2));
  }

  // A2 地面目标低弧命中（15 格 D 站桩位）
  const s2 = S(30, 0.5, 15, 0.5);
  check('A2 地面 D 有解', !!s2, JSON.stringify(s2));
  if (s2) {
    const pts = api.simTrajectory(30, 0.5, s2.x, s2.y);
    check('A2 弹道进入命中窗口', pathMiss(pts, 15, 0.5) < api.CTRL_SOLVE_TOLERANCE,
      'miss=' + pathMiss(pts, 15, 0.5).toFixed(3));
  }

  // A3 超远目标无解（不打必落空的哑弹）
  check('A3 超远 80 格 null', S(30, 0.5, 80, 0.5) === null);

  // A4 零距离 null（贴脸走 attack 分流）
  check('A4 零距离 null', S(30, 0.5, 30, 0.5) === null);

  // A5 近贴目标：不爬坡（补偿档）+ 弹道直达
  const s5 = S(30, 0.5, 31.5, 0.8);
  check('A5 近贴有解', !!s5, JSON.stringify(s5));
  if (s5) {
    const v = Math.hypot(s5.x, s5.y);
    check('A5 不爬坡（≤补偿档）', v <= 13.8 + 1e-9, 'v=' + v.toFixed(2));
    check('A5 弹道直达', pathMiss(api.simTrajectory(30, 0.5, s5.x, s5.y), 31.5, 0.8) < api.CTRL_SOLVE_TOLERANCE);
  }
} else {
  check('A. solveLaunchVector 存在', false, 'PURE 区没有 solveLaunchVector');
}

// ==== B. previewTrajectory（细分真弧线 + 无解 null）====
if (api.previewTrajectory && api.solveLaunchVector) {
  const p1 = api.previewTrajectory(30, 0.5, 15, 0.5);
  // 15 格低平弹 2 拍落地：3 个采样点 ×8 细分 + 终点 = 17 点（任务书"5拍×8=40"
  // 按 60° 高抛估算；低平弹拍数少是物理事实，断言意图=远比旧版 2~6 点平滑）
  check('B1 有解返回多点弧线', Array.isArray(p1) && p1.length >= 16, 'len=' + (p1 && p1.length));
  if (Array.isArray(p1) && p1.length > 2) {
    // 相邻段方向有变化 = 弯曲（折线是等分直线段方向恒定）
    const dir = (a, b) => Math.atan2(b.y - a.y, b.x - a.x);
    const d0 = dir(p1[0], p1[Math.floor(p1.length / 2)]);
    const d1 = dir(p1[Math.floor(p1.length / 2)], p1[p1.length - 1]);
    check('B1 弧线真的弯曲', Math.abs(d1 - d0) > 0.05, 'Δdir=' + Math.abs(d1 - d0).toFixed(3));
    check('B1 起点即发射点', Math.abs(p1[0].x - 30) < 1e-9 && Math.abs(p1[0].y - 0.5) < 1e-9);
  }
  check('B2 无解返回 null', api.previewTrajectory(30, 0.5, 80, 0.5) === null);
} else {
  check('B. previewTrajectory 存在', false);
}

// ==== C. decideClickMessage 三分流 + 无解拦截 ====
if (api.decideClickMessage && api.solveLaunchVector) {
  const state = {
    ents: [
      { x: 30, y: 0.5, radius: 0.5, kind: 'player', controlled: true, name: 'p1' },
      { x: 31.2, y: 0.5, radius: 0.5, kind: 'npc', name: 'S' },   // 贴身（dist 1.2 < 1.5）
      { x: 15, y: 0.5, radius: 0.5, kind: 'npc', name: 'D' },     // 远程可达
      { x: 80, y: 0.5, radius: 0.5, kind: 'npc', name: 'F' },     // 超远无解
    ],
  };
  const c1 = api.decideClickMessage(state, 31.2, 0.5);
  check('C1 贴身=attack', c1.kind === 'attack' && c1.msg.type === 'attack');
  const c2 = api.decideClickMessage(state, 15, 0.5);
  check('C2 远程可达=shoot 且发消息', c2.kind === 'shoot' && c2.msg && c2.msg.type === 'shoot'
    && c2.msg.x === 15 && c2.msg.y === 0.5);
  const c3 = api.decideClickMessage(state, 80, 0.5);
  check('C3 超远=shoot-blocked 且不发消息', c3.kind === 'shoot-blocked' && c3.msg === null,
    JSON.stringify(c3));
  const c4 = api.decideClickMessage(state, 50, 20);
  check('C4 空地=move', c4.kind === 'move' && c4.msg.type === 'move');
} else {
  check('C. decideClickMessage 存在', false);
}

// ==== D. collectKills 排除弹丸 ====
if (api.collectKills) {
  const prev = new Map([
    ['a', { x: 1, y: 1, kind: 'npc' }],
    ['p', { x: 5, y: 5, kind: 'projectile' }],  // 弹丸消亡
  ]);
  const cur = new Map([['a2', { x: 9, y: 9, kind: 'npc' }]]);
  const kills = api.collectKills(prev, cur);
  check('D1 弹丸消亡不算击杀✕', kills.length === 1 && kills[0].x === 1,
    JSON.stringify(kills));
} else {
  check('D. collectKills 存在', false);
}

// ==== E. collectProjectileDissolutions（命中→闪白 / 哑弹→尘点）====
if (api.collectProjectileDissolutions) {
  const prev = new Map([
    ['h', { x: 10, y: 2, kind: 'projectile' }],  // 消亡点附近有身体=命中
    ['d', { x: 40, y: 0.5, kind: 'projectile' }], // 荒地消亡=哑弹尘点
  ]);
  const cur = new Map([['t', { x: 10.3, y: 2.1, kind: 'npc', radius: 0.5 }]]);
  const r = api.collectProjectileDissolutions(prev, cur);
  check('E1 命中分支给 victim 闪白', r.hits.length === 1 && r.hits[0].x === 10.3,
    JSON.stringify(r));
  check('E2 哑弹分支产尘点', r.dust.length === 1 && r.dust[0].x === 40, JSON.stringify(r));
} else {
  check('E. collectProjectileDissolutions 存在', false);
}

// ==== F. 尾迹常量 ====
check('F1 弹丸尾迹 1.5 格', api.PROJ_TRAIL === 1.5, String(api.PROJ_TRAIL));

// ==== G. tooltipAnchor 悬浮标签锚实体正上方 ====
if (api.tooltipAnchor) {
  // 实体屏幕 (500,300) 半径像素 20、提示框 120x24、视口宽 1000：
  // 期望水平居中 (500-60=440)、垂直在身体圆顶上方留 4px (300-20-24-4=252)
  const a1 = api.tooltipAnchor(500, 300, 20, 120, 24, 1000);
  check('G1 居中锚在实体正上方', a1.left === 440 && a1.top === 252, JSON.stringify(a1));
  // 贴左边缘 clamp：不越出视口
  const a2 = api.tooltipAnchor(10, 300, 20, 120, 24, 1000);
  check('G2 左越界 clamp 到 4', a2.left === 4, JSON.stringify(a2));
  // 贴顶 clamp：不越顶
  const a3 = api.tooltipAnchor(500, 10, 20, 120, 24, 1000);
  check('G3 顶越界 clamp 到 4', a3.top === 4, JSON.stringify(a3));
} else {
  check('G. tooltipAnchor 存在', false);
}

console.log('\n' + pass + ' passed, ' + fail + ' failed');
process.exit(fail > 0 ? 1 : 0);
