# CodeWorld 项目开发规则

## 项目概述

CodeWorld 是"代码即现实"的开放世界游戏框架。
框架提供五个存在性原语（时间/空间/渲染/物理规则/沙箱通道），其余一切由玩家代码定义。
技术栈：Rust + wasmtime + WebAssembly + SQLite。

技术方案文档：`docs/architecture.md`（权威设计文档，所有实现以此为准）

## 开发铁律

### 1. 先读架构文档
开工前必须读 `docs/architecture.md` 对应章节。不要凭记忆写代码，文档是唯一权威。

### 2. TDD 必须激活
每个crate开发必须遵循 RED-GREEN-REFACTOR：
- 先写测试（红灯）
- 再写实现（绿灯）
- 最后重构

### 3. 模块边界不可逾越
- 子模块对外只暴露接口，内部实现不对外暴露
- 改实现不能影响其他模块
- 改接口才需要通知所有调用方
- 禁止跨模块直接读对方内部数据结构

### 4. 能力 vs 规则
- 能力（框架提供）：probe能探测、attack能覆写、render能画
- 规则（玩家代码定义）：怎么用、谁能用、代价多大
- 写框架代码时问自己：这是能力还是规则？如果是规则，不应该写在框架里

### 5. 日志必须加
- 新增函数必须有日志
- 修改函数必须有日志
- 数据流两端必须有日志
- bug修复必须加日志说明修复点

### 6. 禁止过度设计
- MVP阶段只做文档里标注 ✅ 的功能
- 标注 ❌ 的功能不实现
- 不提前抽象，不提前优化

## 开发顺序

Phase 1 MVP 按以下顺序推进：

1. `crates/core` -- world-core：WorldState、Entity、EntityId、SurvivalPoints、InstructionGuard
2. `crates/time` -- time-engine：tick计数器、基础快照
3. `crates/space` -- space-engine：2D坐标系、基础get_pos/set_pos
4. `crates/physics` -- physics：默认物理规则（重力/碰撞/地面约束）+ 覆盖接口
5. `crates/arena` -- code-arena：WASM沙箱管理、probe/attack/call通道
6. `crates/render` -- renderer：ASCII渲染、图层排序
7. `crates/storage` -- storage：SQLite持久化
8. `bin/codeworld` -- 主程序：终端运行器

## 技术约束

- Rust edition 2021
- wasmtime v25+
- 异步用 tokio，WASM执行用 spawn_blocking
- 存储MVP阶段全用 SQLite（rusqlite bundled）
- 玩家代码语言 AssemblyScript（但不限制，任何能编译WASM的语言都行）
- 内存限制：每个WASM实例最大64MB
- tick超时：每实体100ms

## 当前阻塞点

attack覆写机制在WASM中不可行（三模型共识），三条候选路径待spike验证：
- 路线A：整模块替换（简单但丢状态）
- 路线B：funcref表间接调用（需定制编译）
- 路线C：Host Hook拦截（只改返回值不改函数体）

spike验证完成前，attack功能用路线A（整模块替换）作为临时实现。

## Git 规范

- 每个功能一个commit
- commit message格式：`feat(module): 描述` 或 `fix(module): 描述`
- 测试通过才能commit
