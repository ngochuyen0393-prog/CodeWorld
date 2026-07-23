// flower.as - 种花型玩家代码（和平生物）
//
// 设计思路：倒计时成熟（tick 计数模拟"10 小时成长"，1 天 = 86400 tick），
// 成熟后通过 render 改外观。被攻击不拦截（和平生物）。读 get_survival 自检存活。
//
// 伪代码级别：导出函数签名对齐架构文档 3.1，import API 调用对齐 3.2，不保证完全可编译。
// 编译（Phase 2）：asc flower.as --outFile flower.wasm --runtime stub

import { tick, get_pos, get_survival, render } from "codeworld";

// 成熟阈值：10 小时。架构文档 4.6 时间成长流程，1 天 = 86400 tick
// 10 小时 = 86400 * 10 / 24 = 36000 tick
const MATURE_TICKS: u64 = 36000;
let matured: bool = false;

// === 必须：存活检查 ===
export function life(): bool {
  // 和平生物：只要还有生存点数就活着
  return get_survival() > 0;
}

// === 必须：每 tick 主逻辑 ===
export function on_tick(world: WorldHandle): void {
  const now = tick();
  // 倒计时成熟：用 tick 计数模拟 10 小时成长
  if (!matured && now >= MATURE_TICKS) {
    matured = true;
  }

  // 渲染：未成熟是种子（·），成熟是花（✿），外观随成熟状态变
  render({
    layer: RenderLayer.Entity,
    y_sort_value: get_pos()[1],
    appearance: matured
      ? { glyph: "✿", color: "pink", size: 8 }
      : { glyph: "·", color: "green", size: 4 },
    pos: get_pos(),
    action_state: matured ? ActionState.Idle : ActionState.Growing,
    priority: 0,
  });
}

// === 可选：被探测时返回成熟状态信息 ===
export function on_probe(request: Bytes): Bytes {
  // 和平生物不藏数据，如实返回成熟度（探测者可据此判断是否可收割）
  const status = matured ? "mature" : "growing";
  return String.UTF8.encode('{"state":"' + status + '"}');
}

// === 可选：被攻击不拦截（和平生物） ===
export function on_attack(payload: Bytes): AttackResult {
  // 不导出本函数时 attack 直接作用 life()；这里显式返回"不拦截"，
  // 让框架按默认流程处理（payload 覆写 life）
  return { intercepted: false, damage: 0 };
}
