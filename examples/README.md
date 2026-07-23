# CodeWorld 玩家代码示例

本目录下的三个 `.as` 文件是 **AssemblyScript** 源码，展示 CodeWorld 玩家 API 的用法。

## 定位

这些文件是 **API 文档和测试素材**，不是可运行产物：

- **MVP 阶段（Phase 1）不执行**：框架还没接入 WASM 执行引擎，玩家代码不会被编译或运行。
- **Phase 2 接入 WASM 执行后**：可用 AssemblyScript 编译器编译为 `.wasm`，由 `code-arena` 沙箱加载执行。
- 代码为 **伪代码级别**：API 调用对齐 `docs/architecture.md` 3.1（玩家导出函数）与 3.2（框架 import API），但不保证完全可编译（部分工具函数仅给出占位实现）。

## 三个示例

| 文件 | 类型 | 设计思路 |
|------|------|---------|
| `attacker.as` | 攻击型 | 视距感知找最近目标 → 探测积累模式 → `attack_at` 打坐标；`on_probe` 撒谎欺骗探测者，`on_attack` 拦截反制 |
| `defender.as` | 防御型 | `register_physics_override` 免疫重力 + 巡逻移动；`on_probe` 加密返回，`on_attack` 多层拦截（校验/混淆/反制） |
| `flower.as` | 种花型（和平） | tick 计数模拟 10 小时成熟，`render` 随成熟度变外观；`on_probe` 如实返回状态，`on_attack` 不拦截 |

每个文件头部注释说明了设计思路，导出函数签名遵循架构文档 3.1 节，import API 调用遵循 3.2 节。文件中出现的 `EntityId` / `ProbeResult` / `AttackResult` / `PhysicsOverrideId` / `RenderLayer` 等类型为框架提供的对外类型，AssemblyScript 编译时由框架侧类型定义补充。

## API 参考

- 玩家导出函数（`life` / `on_tick` / `on_probe` / `on_attack` 等）：`docs/architecture.md` 3.1 节
- 框架提供给玩家代码的 import API（`get_visible_entities` / `attack_at` / `probe` / `render` / `register_physics_override` / `get_survival` 等）：`docs/architecture.md` 3.2 节

## 编译方式（Phase 2）

```bash
asc attacker.as --outFile attacker.wasm --runtime stub
asc defender.as --outFile defender.wasm --runtime stub
asc flower.as    --outFile flower.wasm    --runtime stub
```

- `--runtime stub`：AssemblyScript 最小运行时，适合无标准库的 WASM 模块（CodeWorld 沙箱不提供完整 stdlib）。
- 编译产物 `.wasm` 由 `code-arena` 的 `SandboxManager::create_sandbox` 加载实例化（见 `crates/arena`）。
