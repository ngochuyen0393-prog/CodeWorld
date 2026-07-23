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

Phase 1 MVP 按以下顺序推进（全部完成，271 测试全绿）：

1. `crates/core` ✅ (93测试) -- world-core：WorldState、Entity、EntityId、SurvivalPoints、InstructionGuard
2. `crates/time` ✅ (16测试) -- time-engine：tick计数器、基础快照
3. `crates/space` ✅ (30测试) -- space-engine：2D坐标系、基础get_pos/set_pos
4. `crates/physics` ✅ (28测试) -- physics：默认物理规则（重力/碰撞/地面约束）+ 覆盖接口
5. `crates/arena` ✅ (23测试) -- code-arena：WASM沙箱管理、probe/attack/call通道
6. `crates/render` ✅ (36测试) -- renderer：ASCII渲染、图层排序（借鉴寒霜FrameGraph三阶段管线）
7. `crates/storage` ✅ (26测试) -- storage：SQLite持久化
8. `bin/codeworld` ✅ (9测试) -- 主程序：终端运行器（借鉴Screeps两阶段tick+AzerothCore管理器链）

## MVP 核心缺口（P0/P1 全部完成，2026-07-23/24）

| 缺口 | 状态 | 实现 |
|------|------|------|
| WASM 玩家代码执行 | ✅ | EntitySandboxMap + tick 阶段一调 on_tick/life |
| PendingChanges 队列 | ✅ | core/pending.rs + tick 阶段二 apply |
| 生存点数扣减 | ✅ | tick 阶段二 decay_survival |
| 接触触发沙箱通道 | ✅ | tick 阶段二 detect_collisions + handle_collisions |
| 终端 ANSI 渲染输出 | ❌ Phase 2 | 渲染图算出 diff 但不写终端 |
| 击杀掠夺 | ❌ Phase 2 | 未实现 |
| 多线程并行执行 | ❌ Phase 2 | MVP 单线程 |

## 设计文档索引

- `docs/architecture.md` -- 权威技术方案文档（v2.1）
- `docs/physics-v2-design.md` -- 物理引擎 V2 设计（借鉴 PhysX 5 & Chaos，Phase 2）
- `docs/render-design.md` -- 渲染模块设计（借鉴寒霜 FrameGraph）
- `docs/main-design.md` -- 主程序设计（借鉴 Screeps/AzerothCore/Skynet/Bevy）

## 技术约束

- Rust edition 2021
- wasmtime v25+
- 异步用 tokio，WASM执行用 spawn_blocking
- 存储MVP阶段全用 SQLite（rusqlite bundled）
- 玩家代码语言 AssemblyScript（但不限制，任何能编译WASM的语言都行）
- 内存限制：每个WASM实例最大64MB
- tick超时：每实体100ms

## attack覆写方案（spike已验证）

2026-07-21完成三条路线技术spike验证，16测试全通过。

MVP只用路线A（一个方案搞定所有场景）：
- 路线A（整模块替换）是唯一attack机制：销毁旧实例+用payload重建
- 玩家attack：payload来自攻击者（击杀/击晕/绑架都由payload内容决定，框架不管）
- 框架强制死亡：payload来自框架（life()=false最小模块）
- 路线B（funcref表）留作Phase 2：需定制编译工具链

spike代码在 spike/ 目录下。

## Git 规范

- 每个功能一个commit
- commit message格式：`feat(module): 描述` 或 `fix(module): 描述`
- 测试通过才能commit

<!-- code-graph-mcp:begin v2 -->
## Code Graph (repo-wide AST index)

AST + FTS + vector index of the whole repo — prefer over multi-round Grep/Read for
structural queries (LSP only sees open files; this sees everything). Fastest path = Bash CLI:

| Intent | Command |
|--------|---------|
| Who calls X / what X calls | `code-graph-mcp callgraph X` |
| Impact before editing a fn | `code-graph-mcp impact X` |
| Unfamiliar dir / module | `code-graph-mcp overview <dir>` |
| Symbol source / signature | `code-graph-mcp show X` |
| Concept search (no exact name) | `code-graph-mcp search "…"` (vector: MCP `semantic_code_search`) |
| grep + AST context | `code-graph-mcp grep "pat" [paths] [-t lang] [-g glob] [-c]` |

Still use Grep for literal strings/regex in non-code files; still Read files you'll edit.
Full command + MCP-tool table: `.claude/plugin_code_graph_mcp.md`
<!-- code-graph-mcp:end -->
