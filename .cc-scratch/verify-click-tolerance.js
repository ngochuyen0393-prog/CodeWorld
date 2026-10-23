// 期四第七刀：前端点击选中容差 node 纯逻辑验证（TDD 机器验证）
// 从 index.html 提取纯函数（entityHitTest/nearestEntityAtWorld/decideClickMessage），
// 断言容差语义：活动实体距圆心 <1.5 即选中（取最近）、creation 仍 AABB、自己排除。
let pass = 0, fail = 0;
function check(name, cond, detail) {
  if (cond) { pass++; console.log('PASS ' + name); }
  else { fail++; console.log('FAIL ' + name + (detail ? ' —— ' + detail : '')); }
}

const fs = require('fs');
const html = fs.readFileSync('bin/codeworld/static/index.html', 'utf8');
// #036 起纯函数收拢进 [PURE] 区（decideClickMessage 依赖 meleeInReach/findControlled，
// 抽取范围改 PURE-BEGIN..PURE-END）
const start = html.indexOf('// ==== [PURE-BEGIN]');
const end = html.indexOf('// ==== [PURE-END]');
if (start < 0 || end < 0 || end <= start) {
  console.error('提取锚点没找到（index.html 纯函数区被移动了？）');
  process.exit(2);
}
const src = html.slice(start, end);
// FIX-2 修 3 环境修复：PURE 区 terrainFirstContact 引用的全局 terrainData 声明在
// PURE 区外（index.html :346 IIFE 顶层 var），抽取式验证缺声明 ReferenceError——
// 注入同款声明（null=平地/旧协议，与浏览器运行时初值一致；产品代码无罪）
const factory = new Function('var terrainData = null;\n' + src + '\nreturn { nearestEntityAtWorld: typeof nearestEntityAtWorld !== "undefined" ? nearestEntityAtWorld : null, meleeInReach: typeof meleeInReach !== "undefined" ? meleeInReach : null, meleeInReachEnt: typeof meleeInReachEnt !== "undefined" ? meleeInReachEnt : null, decideClickMessage: typeof decideClickMessage !== "undefined" ? decideClickMessage : null };');
const api = factory();

// === 测试世界 ===
// P 自己（操控目标）@ (0,0)；S @ (10,10)；F @ (11.4,10)（均 r=0.5，相距 1.4 都在彼此容差内）
// D @ (30,10)（远处）；塔 T @ (20,20)
const state = {
  ents: [
    { x: 0, y: 0, radius: 0.5, kind: 'player', controlled: true, name: 'p1' },
    { x: 10, y: 10, radius: 0.5, kind: 'npc', name: 'S' },
    { x: 11.4, y: 10, radius: 0.5, kind: 'npc', name: 'F' },
    { x: 30, y: 10, radius: 0.5, kind: 'npc', name: 'D' },
    { x: 20, y: 20, radius: 1, kind: 'creation', name: 'T', owner: 'p1' },
  ],
};

// 1. 容差命中（RED 点）：点 (11.2,10) 距 S 圆心 1.2、距 F 圆心 0.2——
//    均不在任一身体内（0.2>0.5? 否——F 距 0.2 在 F 身体内！）
//    改点 (12.1,10)：距 S 2.1（容差外）、距 F 0.7（仅容差内）-> 应选 F
{
  const hit = api.nearestEntityAtWorld(state, 12.1, 10);
  check('容差0.7格命中F（身体外容差内）', hit && hit.name === 'F',
    hit ? '选中了 ' + hit.name : '未命中（判定仍严格圆内）');
}
// 2. 容差命中 S：点 (11.2,10) 距 S 1.2、距 F 0.2（F 身体内）-> 最近者 F
//    专门验 S：点 (9.2,10) 距 S 0.8（仅容差内）、距 F 2.2（容差外）-> 选 S
{
  const hit = api.nearestEntityAtWorld(state, 9.2, 10);
  check('容差0.8格命中S', hit && hit.name === 'S',
    hit ? '选中了 ' + hit.name : '未命中（判定仍严格圆内）');
}
// 3. 容差边界外：点 (11.8,10) 距 F 0.4? 不——距 F=|11.8-11.4|=0.4 在 F 身体内。
//    验容差外用 (12.8,10)：距 F 1.4（容差内！）。用 (8.4,10)：距 S 1.6（>1.5 容差外）-> 未命中
{
  const hit = api.nearestEntityAtWorld(state, 8.4, 10);
  check('容差外1.6格不选中', hit === null, hit ? '误选中 ' + hit.name : '');
  const dec = api.decideClickMessage(state, 8.4, 10);
  check('容差外点击=move消息', dec.msg.type === 'move' && dec.msg.x === 8.4 && dec.msg.y === 10,
    JSON.stringify(dec.msg));
}
// 4. 圆身上直接点（d=0）：照旧命中（回归锚）
{
  const hit = api.nearestEntityAtWorld(state, 10, 10);
  check('圆心直点命中S', hit && hit.name === 'S', hit ? '选中 ' + hit.name : '未命中');
}
// 5. 多命中取最近（两者都仅在容差内）：点 (11.9,10) 距 F 0.5、距 S 1.9 ->
//    F 0.5 最近选中；点 (10.9,10) 距 S 0.9、距 F 0.5（0.5=半径边界，严格圆内不含）
//    ——两者都仅容差内，取最近 F
{
  const hit = api.nearestEntityAtWorld(state, 10.9, 10);
  check('双容差命中取最近F', hit && hit.name === 'F',
    hit ? '选中 ' + hit.name : '未命中');
}
// 6. creation 保持 AABB：点塔身内 (20.5,20.5) 命中；点塔身外 (21.9,20.5)
//    （距 AABB 边 0.9、在 1.5 容差内但 creation 不吃容差）不命中
{
  const hitIn = api.nearestEntityAtWorld(state, 20.5, 20.5);
  check('creation塔身内命中', hitIn && hitIn.name === 'T', hitIn ? '选中 ' + hitIn.name : '未命中');
  const hitOut = api.nearestEntityAtWorld(state, 21.9, 20.5);
  check('creation塔身外不吃容差', hitOut === null, hitOut ? '误选中 ' + hitOut.name : '');
  const dec = api.decideClickMessage(state, 21.9, 20.5);
  check('塔外点击=move消息', dec.msg.type === 'move', JSON.stringify(dec.msg));
}
// 7. 自己排除：点自己圆心 (0,0) -> 不出刀（move 语义）
{
  const hit = api.nearestEntityAtWorld(state, 0, 0);
  check('操控目标自己排除', hit === null, hit ? '误选中 ' + hit.name : '');
  const dec = api.decideClickMessage(state, 0, 0);
  check('点自己=move消息', dec.msg.type === 'move', JSON.stringify(dec.msg));
}
// 8. 命中 -> 消息带实体快照坐标（不是点击坐标）：点 (9.2,10) 容差命中 S ->
//    #036 三分流：S 距 P(0,0) 远超贴身判距 -> shoot 消息，坐标仍取 S 快照 (10,10)
//    （旧语义"命中即 attack"已被三分流取代：贴身才 attack，远程=shoot）
{
  const dec = api.decideClickMessage(state, 9.2, 10);
  check('命中=shoot消息带实体快照坐标（#036 远程）',
    dec.kind === 'shoot' && dec.msg.type === 'shoot' && dec.msg.x === 10 && dec.msg.y === 10 &&
    dec.fx && dec.fx.x === 10,
    JSON.stringify(dec));
}

// 9. FIX-2 贴身门身体化（前端镜像，实测土豆 1023 F 案复刻）：
//    P 悬停 1×1 塔上方贴脸 @ (34.93,1.58)，塔（creation static）pos=(35,0)
//    AABB [35,36]×[0,1]（floor 推导）。圆心距≈1.58 > 旧判距 1.5（旧镜像
//    落空走 shoot），身体感知距离（圆到 AABB 最近点 (35,1.0)）≈0.584 < 1.0
//    （贴身够得着走 attack）——前端分流必须与服务器 melee_in_reach_bodies
//    同判据（#028 镜像铁律）
{
  const hoverState = {
    ents: [
      { x: 34.93, y: 1.58, radius: 0.5, kind: 'player', controlled: true, name: 'p1' },
      { x: 35, y: 0, radius: 0.5, kind: 'creation', name: 'T', owner: 'p2' },
    ],
  };
  // 点击塔身内 (35.5,0.5)：nearestEntityAtWorld 格判据命中塔
  const hit = api.nearestEntityAtWorld(hoverState, 35.5, 0.5);
  check('贴脸悬停点塔身内命中塔', hit && hit.name === 'T', hit ? '选中 ' + hit.name : '未命中');
  // 旧圆心距镜像在实测场景落空的锚（判据错配证据，改后必须仍成立）：
  const legacy = api.meleeInReach(34.93, 1.58, 0.5, 35, 0, 0.5);
  check('旧圆心距镜像同场景落空（判据错配锚）', legacy === false, String(legacy));
  // 分流断言：修后走 attack（红框+刀光线反馈），与服务器接触门一致
  const dec = api.decideClickMessage(hoverState, 35.5, 0.5);
  check('贴脸悬停塔上方点塔=attack分流（身体感知镜像）',
    dec.kind === 'attack' && dec.msg.type === 'attack' && dec.msg.x === 35 && dec.msg.y === 0,
    JSON.stringify(dec));
}

console.log('\n=== 结果: ' + pass + ' 过 / ' + fail + ' 败 ===');
process.exit(fail > 0 ? 1 : 0);
