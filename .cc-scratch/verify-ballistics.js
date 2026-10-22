// #034 弹道系统前端 PURE 区机器验证（node 桩环境；函数与 index.html PURE 区同源，
// 改一处必须同步另一处——与第七刀容差验证脚本同模式）
'use strict';
var pass = 0, fail = 0;
function ok(cond, name) {
  if (cond) { pass++; console.log('  ok  ' + name); }
  else { fail++; console.log('FAIL  ' + name); }
}

// ==== 与 index.html PURE 区同源的函数（复制自 [PURE-BEGIN]..[PURE-END]）====
var MELEE_REACH = 0.5;
function meleeInReach(ax, ay, ar, bx, by, br) {
  var dx = ax - bx, dy = ay - by;
  var s = ar + br + MELEE_REACH;
  return dx * dx + dy * dy < s * s;
}
function collectProjectileHits(prevEnts, newEnts) {
  var out = [];
  prevEnts.forEach(function (e, id) {
    if (e.kind === 'projectile' && !newEnts.has(id)) out.push({ x: e.x, y: e.y });
  });
  return out;
}
function nearestProjectileVictim(entsMap, x, y) {
  var hit = null, hd = 6.0 * 6.0;
  entsMap.forEach(function (e) {
    if (e.kind === 'projectile') return;
    var dx = e.x - x, dy = e.y - y;
    var d = dx * dx + dy * dy;
    if (d >= hd) return;
    hit = e; hd = d;
  });
  return hit;
}
function nearestEntityAtWorld(state, wx, wy) {
  var CLICK_TOLERANCE = 1.5;
  if (!state) return null;
  var hit = null, hd = 0;
  state.ents.forEach(function (e) {
    if (e.controlled === true) return;
    if (e.kind === 'projectile') return;
    var dx = e.x - wx, dy = e.y - wy;
    if (e.kind === 'creation') {
      var mx = Math.floor(e.x), my = Math.floor(e.y);
      if (!(wx >= mx && wx <= mx + 1 && wy >= my && wy <= my + 1)) return;
    } else {
      if (dx * dx + dy * dy >= CLICK_TOLERANCE * CLICK_TOLERANCE) return;
    }
    var d = dx * dx + dy * dy;
    if (!hit || d < hd) { hit = e; hd = d; }
  });
  return hit;
}

// ==== meleeInReach（#034 贴身判距，与 collision.rs 严格 < 同判据）====
ok(meleeInReach(0, 0, 0.5, 1.49, 0, 0.5) === true, '判距内 1.49 < 1.5 = 够得着');
ok(meleeInReach(0, 0, 0.5, 1.5, 0, 0.5) === false, '恰在判距 1.5 = 落空（严格 < 锚定）');
ok(meleeInReach(0, 0, 0.5, 1.51, 0, 0.5) === false, '判距外 1.51 = 落空');
ok(meleeInReach(0, 0, 0.5, 1.0, 0, 0.5) === true, '1 格外敌人够得着（身体相触+半格臂展）');
ok(meleeInReach(0, 0, 2.0, 2.9, 0, 0.5) === true, '大身体判距 3.0 内');
ok(meleeInReach(0, 0, 2.0, 3.0, 0, 0.5) === false, '大身体恰在 3.0 = 落空');
ok(meleeInReach(0, 0, 0.5, 0.9, 0.9, 0.5) === true, '斜向距离 1.27 < 1.5 = 够得着');

// ==== collectProjectileHits（弹丸消亡检测）====
var prev = new Map();
prev.set('p1', { kind: 'projectile', x: 3, y: 2 });
prev.set('e1', { kind: 'npc', x: 5, y: 5 });
var next = new Map();
next.set('e1', { kind: 'npc', x: 5, y: 5 });
var hits = collectProjectileHits(prev, next);
ok(hits.length === 1 && hits[0].x === 3 && hits[0].y === 2, '消失弹丸返回最后坐标');
ok(collectProjectileHits(next, next).length === 0, '无弹丸消失返回空');
prev.set('p2', { kind: 'projectile', x: 9, y: 9 });
ok(collectProjectileHits(prev, next).length === 2, '多弹丸同拍消亡全收集');
prev.set('e1x', { kind: 'npc', x: 7, y: 7 });
ok(collectProjectileHits(prev, next).length === 2, '非弹丸消失不算弹丸命中');

// ==== nearestProjectileVictim（消失点附近受害者）====
var ents = new Map();
ents.set('v1', { kind: 'npc', x: 3.3, y: 2.1 });
ents.set('fly', { kind: 'projectile', x: 3.0, y: 2.0 });
ents.set('far', { kind: 'npc', x: 30, y: 30 });
var v = nearestProjectileVictim(ents, 3, 2);
ok(v && v.kind === 'npc' && v.x === 3.3, '命中点附近取非弹丸最近实体');
ok(nearestProjectileVictim(ents, 20, 20) === null, '附近无人（超时哑弹）返回 null');
ents.set('far6', { kind: 'npc', x: 10.0, y: 6.0 }); // 距 (3,2) sqrt(65)≈8.1 格，在 6 格搜索半径外
ok(nearestProjectileVictim(ents, 3, 2).x === 3.3, '6 格半径内取最近受害者（正常弹速一拍 4-6 格可达）');
ents.set('me', { kind: 'player', x: 3.1, y: 2.0, controlled: true });
ok(nearestProjectileVictim(ents, 3, 2).x === 3.1, '操控目标也算受害者（弹丸可以打你）');

// ==== nearestEntityAtWorld 弹丸排除（点弹丸不出刀）====
var st = { ents: new Map() };
st.ents.set('fly', { kind: 'projectile', x: 5, y: 5 });
st.ents.set('npc', { kind: 'npc', x: 8, y: 5 });
ok(nearestEntityAtWorld(st, 5, 5) === null, '点击弹丸不选中（弹丸非攻击目标）');
ok(nearestEntityAtWorld(st, 8, 5) && nearestEntityAtWorld(st, 8, 5).kind === 'npc', '点击敌人照常选中');

console.log('\n' + pass + ' passed, ' + fail + ' failed');
process.exit(fail > 0 ? 1 : 0);
