// 期五第二刀：登出弹窗修复 node 纯逻辑验证（TDD 机器验证）
// 从 index.html PURE 区提取 findControlled/findDisposedPlayer，
// 断言登出态镜头归属语义：stay_auto/stay_ai 留场按处置状态签找到自己的 P、
// take 带走/被杀返回 null（镜头回落全景）、npc/dispose_wait 不误选、在线态回归锚不变。
let pass = 0, fail = 0;
function check(name, cond, detail) {
  if (cond) { pass++; console.log('PASS ' + name); }
  else { fail++; console.log('FAIL ' + name + (detail ? ' —— ' + detail : '')); }
}

const fs = require('fs');
const html = fs.readFileSync('bin/codeworld/static/index.html', 'utf8');
const start = html.indexOf('// 实体表里找操控目标');
const end = html.indexOf('// 登出处置选项');
if (start < 0 || end < 0 || end <= start) {
  console.error('提取锚点没找到（index.html 纯函数区被移动了？）');
  process.exit(2);
}
const src = html.slice(start, end);
const factory = new Function(src + '\nreturn { findControlled: typeof findControlled !== "undefined" ? findControlled : null, findDisposedPlayer: typeof findDisposedPlayer !== "undefined" ? findDisposedPlayer : null };');
const api = factory();

if (typeof api.findDisposedPlayer !== 'function') {
  console.log('FAIL findDisposedPlayer 不存在（未实现）');
  fail++;
} else {
  // === 测试实体表（Map 与生产路径同构：sampleState 返回的 ents 是 Map）===
  // P pioneer：登出留场态（服务器 #032：处置中的玩家不再标 controlled，带处置状态签）
  function entsMap(list) {
    const m = new Map();
    list.forEach(e => m.set(e.id, e));
    return m;
  }

  // 1. stay_auto 登出态：P 带 disposition=auto、controlled=false → 找到 P（镜头继续跟）
  {
    const m = entsMap([
      { id: '1', kind: 'player', controlled: false, disposition: 'auto', name: 'pioneer' },
      { id: '2', kind: 'npc', name: 'D' },
    ]);
    const me = api.findDisposedPlayer(m);
    check('stay_auto 登出态找到自己的P', me && me.name === 'pioneer',
      me ? '选中 ' + me.name : '返回 null（镜头会丢目标回落全景）');
  }
  // 2. stay_ai 登出态：disposition=ai-await → 找到 P
  {
    const m = entsMap([
      { id: '1', kind: 'player', controlled: false, disposition: 'ai-await', name: 'pioneer' },
    ]);
    const me = api.findDisposedPlayer(m);
    check('stay_ai 登出态找到自己的P', me && me.name === 'pioneer', '');
  }
  // 3. take 带走/被杀：P 不在表 → null（调用方镜头回落全景）
  {
    const m = entsMap([
      { id: '2', kind: 'npc', name: 'D' },
      { id: '3', kind: 'creation', name: 'T' },
    ]);
    const me = api.findDisposedPlayer(m);
    check('P消失返回null（回落全景）', me === null, me ? '误选中 ' + me.name : '');
  }
  // 4. 在线态实体（无 disposition 字段）不选——在线跟随走 findControlled 主路，不抢
  {
    const m = entsMap([
      { id: '1', kind: 'player', controlled: false, name: 'other-player' },
    ]);
    const me = api.findDisposedPlayer(m);
    check('在线玩家（无disposition）不误选', me === null, me ? '误选中 ' + me.name : '');
  }
  // 5. kind 闸门：非 player 实体即使带 disposition 值也不选（服务器契约 npc/creation 无此字段，防御性）
  {
    const m = entsMap([
      { id: '9', kind: 'npc', disposition: 'auto', name: 'S' },
      { id: '8', kind: 'creation', disposition: 'auto', name: 'T' },
    ]);
    const me = api.findDisposedPlayer(m);
    check('非player实体不误选', me === null, me ? '误选中 ' + me.name : '');
  }
  // 6. dispose_wait 不算登出跟随目标（那是意外断线的抢救窗口，不是登出留场态）
  {
    const m = entsMap([
      { id: '1', kind: 'player', controlled: false, disposition: 'dispose_wait', wait_left: 30, name: 'pioneer' },
    ]);
    const me = api.findDisposedPlayer(m);
    check('dispose_wait不作为登出跟随目标', me === null, me ? '误选中 ' + me.name : '');
  }
  // 7. 混合表：P（ai-await）+ 其他玩家（auto）+ npc + creation → 稳定选中（两次调用同结果）
  {
    const m = entsMap([
      { id: '1', kind: 'player', controlled: false, disposition: 'auto', name: 'other' },
      { id: '2', kind: 'npc', name: 'D' },
      { id: '3', kind: 'player', controlled: false, disposition: 'ai-await', name: 'pioneer' },
      { id: '4', kind: 'creation', name: 'T' },
    ]);
    const a = api.findDisposedPlayer(m);
    const b = api.findDisposedPlayer(m);
    check('混合表稳定选中player', a && a.kind === 'player' && a === b,
      a ? '选中 ' + a.name : '返回 null');
  }
}

// 8. 回归锚：在线态 findControlled 照旧（controlled=true 优先主路不动）
{
  const m = new Map();
  m.set('1', { id: '1', kind: 'player', controlled: false, disposition: 'auto', name: 'other' });
  m.set('2', { id: '2', kind: 'player', controlled: true, name: 'pioneer' });
  const me = api.findControlled ? api.findControlled(m) : null;
  check('回归锚：在线态findControlled照旧', me && me.name === 'pioneer',
    me ? '选中 ' + me.name : '返回 null');
}

console.log('---');
console.log('PASS ' + pass + ' / FAIL ' + fail);
process.exit(fail > 0 ? 1 : 0);
