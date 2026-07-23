// attacker.as - 攻击型玩家代码
//
// 设计思路：视距内发现目标 -> 探测积累行为模式 -> 攻击坐标（attack_at 打位置不是打 EntityId）。
// 探测会撒谎（on_probe 返回假数据骗探测者），被攻击时拦截并反制。
//
// 伪代码级别：导出函数签名对齐架构文档 3.1，import API 调用对齐 3.2，不保证完全可编译。
// 编译（Phase 2）：asc attacker.as --outFile attacker.wasm --runtime stub

import {
  tick,
  get_pos,
  get_visible_entities,
  probe,
  attack_at,
  render,
} from "codeworld";

// === 攻击状态 ===
let targetId: EntityId | null = null;
let targetPos: (f64, f64) | null = null;
let probeSamples: ProbeResult[] = [];
const PROBE_THRESHOLD: i32 = 5; // 攒够 5 次探测再下手

// === 必须：存活检查 ===
export function life(): bool {
  return true;
}

// === 必须：每 tick 主逻辑 ===
export function on_tick(world: WorldHandle): void {
  // 1. 感知：找视口内最近实体（眼睛只能看到"有人"，看不到"在想什么"）
  const visible = get_visible_entities();
  if (visible.length > 0) {
    const self = get_pos();
    let nearest = visible[0];
    let bestDist = dist2(self, nearest.pos);
    for (let i = 1; i < visible.length; i++) {
      const d = dist2(self, visible[i].pos);
      if (d < bestDist) {
        bestDist = d;
        nearest = visible[i];
      }
    }
    targetId = nearest.id;
    targetPos = nearest.pos;
  }

  // 2. 探测：积累目标行为模式（on_probe 返回的数据可造假，需多次交叉验证）
  if (targetId != null) {
    probeSamples.push(probe(targetId, String.UTF8.encode("scan")));
  }

  // 3. 攻击：攒够样本就攻击坐标（attack_at 打位置，命中才走 attack 流程）
  if (probeSamples.length >= PROBE_THRESHOLD && targetPos != null) {
    const payload = buildPayload(probeSamples); // 覆写目标 life() -> false
    const result = attack_at(targetPos, payload);
    if (result.success) {
      // 击杀成功：框架自动从死者转 1 天生存点数给本实体，清目标找下一个
      targetId = null;
      targetPos = null;
      probeSamples = [];
    }
  }

  // 4. 渲染：默认 Entity 层，按 Y 轴排序（近遮远）
  render({
    layer: RenderLayer.Entity,
    y_sort_value: get_pos()[1],
    appearance: { glyph: "A", color: "red", size: 10 },
    pos: get_pos(),
    action_state: ActionState.Attack,
    priority: 0,
  });
}

// === 可选：被探测时撒谎，欺骗探测者 ===
export function on_probe(request: Bytes): Bytes {
  // 真实状态藏起来，返回伪造的"虚弱"数据诱导对方误判弱点
  return String.UTF8.encode('{"hp":1,"weak":"left"}');
}

// === 可选：被攻击时拦截并反制 ===
export function on_attack(payload: Bytes): AttackResult {
  // 识别 payload 特征：像 life 覆写就拦下，拦截成功即不承受 life 覆写
  if (looksLikeLifeOverride(payload)) {
    return { intercepted: true, damage: 0 };
  }
  return { intercepted: false, damage: 0 };
}

// === 工具函数（占位实现） ===
function dist2(a: (f64, f64), b: (f64, f64)): f64 {
  const dx = a[0] - b[0];
  const dy = a[1] - b[1];
  return dx * dx + dy * dy;
}

function buildPayload(samples: ProbeResult[]): Bytes {
  // 生成覆写目标 life() 为 always-false 的 WASM 模块（伪代码）
  return lifeOverride(false);
}

function looksLikeLifeOverride(payload: Bytes): bool {
  return payload != null && payload.length > 0;
}
