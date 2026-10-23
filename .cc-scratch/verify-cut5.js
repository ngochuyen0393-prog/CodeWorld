// 期五第五刀（#036）：断线冻结 + 点击三分流（shoot）+ 弹道弧线预览 node 纯逻辑验证
// 从 index.html 提取 [PURE-BEGIN]..[PURE-END] 纯函数区，断言：
//   A. 断线冻结：canClick 门控三态（bad/wait 拦截、ok 放行）+ 断线点击不产生预测/虚线
//   B. 点击三分流：贴身=attack / 远程=shoot（只发目标坐标）/ 空地=move
//   C. 弹道弧线预览：定装初速 12、60° 仰角上抛、逐拍重力+阻力模拟、落地截断
//   D. 断线横幅文案 + 既有镜像函数回归锚（applyMoveSteps/meleeInReach 不回归）
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
   'nearestEntityAtWorld', 'solveLaunchVector', 'previewTrajectory', 'decideClickMessage',
   'canClick', 'dcBannerText', 'CTRL_SHOOT_SPEED', 'MELEE_REACH']
    .map((n) => n + ': typeof ' + n + ' !== "undefined" ? ' + n + ' : null').join(',\n') +
 '\n};');
const api = factory();

// === 测试世界：P 自己 @ (0,0.5)；S 贴身 @ (1.2,0.5)（dist 1.2 < 判距 1.5）；
// D 远处 @ (30,0.5)；T 塔（creation）@ (20,20) ===
const state = {
  ents: [
    { x: 0, y: 0.5, radius: 0.5, kind: 'player', controlled: true, name: 'p1' },
    { x: 1.2, y: 0.5, radius: 0.5, kind: 'npc', name: 'S' },
    { x: 30, y: 0.5, radius: 0.5, kind: 'npc', name: 'D' },
    { x: 20, y: 20, radius: 1, kind: 'creation', name: 'T', owner: 'p1' },
  ],
};

// === A. 断线冻结（#036）===

// A1. canClick 三态：只有"已连接"放行
check('canClick(ok)=true', api.canClick('ok') === true);
check('canClick(bad)=false（断线拦截）', api.canClick('bad') === false);
check('canClick(wait)=false（连接中拦截）', api.canClick('wait') === false);

// A2. 断线态点击不产生预测/虚线/上行（处理器早退的纯逻辑等价物）：
//     canClick 拦截后不应走到 decideClickMessage/startPrediction——模拟完整门控
function simulateClick(conn, st, wx, wy) {
  if (!api.canClick(conn)) return { sent: null, pred: null, dashed: null };
  const d = api.decideClickMessage(st, wx, wy);
  return {
    sent: d.msg,
    pred: d.kind === 'move' ? { x: wx, y: wy } : null, // move 才建预测
    dashed: d.kind === 'move' ? { x: wx, y: wy } : null, // move 才画虚线
  };
}
{
  const bad = simulateClick('bad', state, 30, 0.5);
  check('断线点击：无上行消息', bad.sent === null);
  check('断线点击：无预测目标', bad.pred === null);
  check('断线点击：无虚线目标', bad.dashed === null);
  const badMove = simulateClick('bad', state, 5, 5);
  check('断线点空地：同样无预测（P 不动）', badMove.pred === null && badMove.sent === null);
  const okMove = simulateClick('ok', state, 5, 5);
  check('在线点空地：move+预测照常（回归锚）',
    okMove.sent && okMove.sent.type === 'move' && okMove.pred !== null);
}

// A3. 断线横幅文案
check('断线横幅含警示语+次数', api.dcBannerText(2).indexOf('与世界断开') >= 0 &&
  api.dcBannerText(2).indexOf('第2次') >= 0, api.dcBannerText(2));

// === B. 点击三分流（#036）===

// B1. 贴身点实体 = attack（#027/#034 语义不变）：点 S 圆心，dist 1.2 < 1.5
{
  const d = api.decideClickMessage(state, 1.2, 0.5);
  check('贴身=attack', d.kind === 'attack' && d.msg.type === 'attack', JSON.stringify(d.msg));
  check('attack 带实体快照坐标', d.msg.x === 1.2 && d.msg.y === 0.5);
}
// B2. 远程点实体 = shoot（新语义）：点 D，dist 30 超贴身；前端只发目标坐标
{
  const d = api.decideClickMessage(state, 30, 0.5);
  check('远程=shoot', d.kind === 'shoot' && d.msg.type === 'shoot', JSON.stringify(d.msg));
  check('shoot 只发目标坐标', d.msg.x === 30 && d.msg.y === 0.5);
}
// B3. 空地 = move（不变）
{
  const d = api.decideClickMessage(state, 5, 5);
  check('空地=move', d.kind === 'move' && d.msg.type === 'move' && d.msg.x === 5 && d.msg.y === 5);
}
// B4. 远程点 creation（塔）：AABB 命中但远超贴身。塔在 (20.5,20.5) 距 P 28.5 格
//     ——期五第六刀解算无解 → shoot-blocked 不发消息（新语义；可达目标仍 shoot）
{
  const d = api.decideClickMessage(state, 20.5, 20.5);
  check('远程点塔超射程=shoot-blocked', d.kind === 'shoot-blocked' && d.msg === null, JSON.stringify(d));
}
// B5. 点自己排除（回归锚）：无邻实体世界里点自己圆心 = move
//    （主测试世界里 S 距 P 1.2 在容差内，点 P 圆心会选中 S——那不是"点自己"）
{
  const lone = { ents: [{ x: 0, y: 0.5, radius: 0.5, kind: 'player', controlled: true, name: 'p1' }] };
  const d = api.decideClickMessage(lone, 0, 0.5);
  check('点自己=move（排除不出刀/不出弹）', d.kind === 'move');
}
// B6. 贴身边界：dist 恰 1.5 = 严格 < 落选 attack -> shoot（与服务器 melee_in_reach 同判据）
{
  const st2 = {
    ents: [
      { x: 0, y: 0.5, radius: 0.5, kind: 'player', controlled: true, name: 'p1' },
      { x: 1.5, y: 0.5, radius: 0.5, kind: 'npc', name: 'E' },
    ],
  };
  const d = api.decideClickMessage(st2, 1.5, 0.5);
  check('恰在判距=shoot（严格 <）', d.kind === 'shoot', d.kind);
}

// === C. 弹道解算与弧线预览（期五第六刀解算器语义同步：ctrlShootVector 60° 定角
// 退役，改 solveLaunchVector 速度梯×角度扫描——详见 verify-cut6.js 全量断言）===

// C1. 解算方向性：正东目标 vx>0、正西 vx<0（水平方向不丢）
{
  if (api.solveLaunchVector) {
    const v = api.solveLaunchVector(0, 0.5, 15, 0.5);
    check('正东目标有解且 vx>0', !!v && v.x > 0, JSON.stringify(v));
    const vw = api.solveLaunchVector(0, 0.5, -15, 0.5);
    check('正西目标有解且 vx<0', !!vw && vw.x < 0, JSON.stringify(vw));
  } else {
    check('solveLaunchVector 存在', false);
  }
}
// C2. 弧线预览形状：起点=发射点、落地截断、无解 null（细分平滑断言归 verify-cut6 B 组）
{
  if (api.previewTrajectory && api.solveLaunchVector) {
    const pts = api.previewTrajectory(0, 0.5, 15, 0.5);
    check('弧线起点=发射点', !!pts && pts[0].x === 0 && pts[0].y === 0.5);
    check('弧线多点采样（≥16 点细分）', !!pts && pts.length >= 16, pts && String(pts.length));
    check('x 单调朝目标', !!pts && pts.every((p, i) => i === 0 || p.x > pts[i - 1].x));
    const last = pts[pts.length - 1];
    check('落地截断（末点 y<0.15 或步数用尽）', last.y < 0.15 || pts.length >= 30, JSON.stringify(last));
    check('超远目标弧线 null（不画够不着的弧）', api.previewTrajectory(0, 0.5, 60, 0.5) === null);
  } else {
    check('previewTrajectory 存在', false);
  }
}
// C4. 纯装饰性：弹道预览与服务器无关（无 DOM/WS 依赖即纯函数——本验证本身即证据）

// === D. 既有镜像回归锚（本刀不该碰坏的东西）===

// D1. applyMoveSteps 镜像（预测规则与 input.rs next_move_step 双实现）
{
  const p = api.applyMoveSteps(0, 0.5, 3, 0.5, 2);
  check('applyMoveSteps 两步东行', Math.abs(p.x - 2) < 1e-9 && Math.abs(p.y - 0.5) < 1e-9, JSON.stringify(p));
}
// D2. 贴身判距镜像（collision.rs melee_in_reach）
{
  check('meleeInReach 贴身真', api.meleeInReach(0, 0.5, 0.5, 1.2, 0.5, 0.5) === true);
  check('meleeInReach 恰判距假（严格<）', api.meleeInReach(0, 0.5, 0.5, 1.5, 0.5, 0.5) === false);
}
// D3. 和解决策
{
  check('reconcileDecision 一致确认', api.reconcileDecision(0, 0.5, 3, 0.5, 2, 2, 0.5) === 'confirm');
  check('reconcileDecision 分歧判异', api.reconcileDecision(0, 0.5, 3, 0.5, 2, 9, 0.5) === 'diverge');
}

console.log('\n=== 结果: ' + pass + ' 过 / ' + fail + ' 败 ===');
process.exit(fail > 0 ? 1 : 0);
