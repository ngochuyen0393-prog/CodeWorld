// defender.as - 防御型玩家代码
//
// 设计思路：免疫重力（register_physics_override scope=Entity）巡逻移动，
// 探测返回加密数据，被攻击多层拦截（校验 payload -> 混淆 -> 反制）。
//
// 伪代码级别：导出函数签名对齐架构文档 3.1，import API 调用对齐 3.2，不保证完全可编译。
// 编译（Phase 2）：asc defender.as --outFile defender.wasm --runtime stub

import {
  tick,
  get_pos,
  set_pos,
  get_survival,
  register_physics_override,
  render,
} from "codeworld";

// === 巡逻路径与重力覆盖句柄 ===
const PATROL_POINTS: (f64, f64)[] = [(0.0, 5.0), (10.0, 5.0), (10.0, 15.0), (0.0, 15.0)];
let patrolIdx: i32 = 0;
let gravityOverrideId: PhysicsOverrideId | null = null;

// === 必须：存活检查（带额外校验） ===
export function life(): bool {
  // 框架每 tick 调 life；这里加业务校验：生存点数耗尽或自检失败都算死
  return get_survival() > 0 && selfIntegrityOk();
}

// === 必须：每 tick 主逻辑 ===
export function on_tick(world: WorldHandle): void {
  // 1. 注册物理覆盖：scope=Entity 只影响自己 -> 免疫重力（巡逻时不坠落）
  if (gravityOverrideId == null) {
    gravityOverrideId = register_physics_override(
      PhysicsScope.Entity, // 仅影响自身（"我免疫重力"）
      buildGravityImmunityCode(), // WASM 字节码：把自身 Y 方向加速度置 0
      100, // priority：高优先级覆盖默认重力
    );
  }

  // 2. 巡逻移动：沿路径点走，set_pos 经物理规则校验（碰撞挤压等）与空间变换管线
  const target = PATROL_POINTS[patrolIdx];
  stepToward(target);
  if (reached(target)) {
    patrolIdx = (patrolIdx + 1) % PATROL_POINTS.length;
  }

  // 3. 渲染：防御塔外观
  render({
    layer: RenderLayer.Entity,
    y_sort_value: get_pos()[1],
    appearance: { glyph: "D", color: "blue", size: 12 },
    pos: get_pos(),
    action_state: ActionState.Patrol,
    priority: 1,
  });
}

// === 可选：被探测时加密返回 ===
export function on_probe(request: Bytes): Bytes {
  // 真实内部状态加密后返回，探测者拿到密文需消耗 compute 算力才能解读
  const plain = '{"hp":100,"shield":true}';
  return encrypt(plain);
}

// === 可选：被攻击多层拦截 ===
export function on_attack(payload: Bytes): AttackResult {
  // 层1：校验 payload 合法性（非法直接吞，不泄露信息）
  if (!validatePayload(payload)) {
    return { intercepted: true, damage: 0 };
  }
  // 层2：混淆 -- 让对方以为命中，实际偏移伤害（伪代码省略）
  // 层3：反制 -- 拦截成功则不承受 life 覆写
  return { intercepted: true, damage: 0 };
}

// === 工具函数（占位实现） ===
function selfIntegrityOk(): bool {
  // 自检代码完整性（伪代码：校验关键内存段未篡改）
  return true;
}

function buildGravityImmunityCode(): Bytes {
  // 返回置零 Y 加速度的 WASM 字节码（伪代码）
  return gravityNullifier();
}

function stepToward(target: (f64, f64)): void {
  const cur = get_pos();
  set_pos(approach(cur[0], target[0]), approach(cur[1], target[1]));
}

function reached(target: (f64, f64)): bool {
  const cur = get_pos();
  return Math.abs(cur[0] - target[0]) < 0.5 && Math.abs(cur[1] - target[1]) < 0.5;
}

function approach(cur: f64, target: f64): f64 {
  if (cur < target) return cur + 1.0;
  if (cur > target) return cur - 1.0;
  return cur;
}

function encrypt(plain: string): Bytes {
  // 伪代码：真实用 compute 提交加密任务
  return String.UTF8.encode(plain);
}

function validatePayload(payload: Bytes): bool {
  // payload 上限 64KB（架构 3.3 attack_payload_max_size）
  return payload != null && payload.length < 64 * 1024;
}
