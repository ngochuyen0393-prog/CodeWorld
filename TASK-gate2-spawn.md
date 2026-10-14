# TASK（轻装挡）：期二·门二开放——spawn/destroy 开放给玩家代码 + 产权链

## ⚠️ 必须使用 test-driven-development（红→绿）

## 宪法依据

docs/world-foundation.md（门二：代码 spawn；产权链）+ decisions.md #012（能力统一，造物再造造物合法）+ #015（产权链记录归属）。

## 项目上下文

项目：D:/projects/CodeWorld（Rust workspace，基线 457 测试全绿，git 37a8911）
已有地基（实查过）：
- `PendingChange::Spawn { entity }` / `Destroy { entity: EntityId }`：队列+阶段二应用通路完整（main.rs:732）
- `Entity.owner: PlayerId` 字段已存在（entity.rs:94）——产权链锚点
- `EntitySandboxMap`：沙箱创建/销毁已有；`create(entity_id, wasm, fuel)`
- 玩家进场已有（players/ 目录，PlayerDirectory）
- api.md v1 已有 spawn/destroy 在路线图章节
- sandbox.rs env import 注册模式：func_wrap("env", "名字", closure)；现有 8 个 import
- 沙箱内拿不到 world：通路都是"SandboxData 收集请求 → 阶段二统一应用"（参照 move_by 的 pending_moves 模式）

## 需求

### A. 新 import：spawn
`env.spawn(code_ptr: i32, code_len: i32, x: f64, y: f64, glyph_ptr: i32, color_ptr: i32) -> i32`
- 语义：玩家代码提交一段 WAT 文本 + 出生坐标 + 外观（glyph 单字符、color 字符串），请求造一个新实体（造物）
- 通路（严格对齐 move_by 模式）：import 闭包把请求存进 SandboxData 新字段 pending_spawns → main 阶段二统一处理
- 位置起点限制：造物出生坐标必须在【创造者自身坐标 ±10 格】内（防远程投射刷屏，MVP 一刀切的简单规则，注释标注"规则由宪法允许后续玩法化"）
- 数量限制：每个实体每 tick 最多 1 个 spawn 请求（多余的拒绝+warn）；世界实体总数上限 64（Config 可调 CODEWORLD_MAX_ENTITIES，默认 64）——达上限拒绝+warn
- 造物实体构造：新 EntityId、owner=创造者的 owner（产权链）、code_hash="creation"、Appearance(glyph, color)、出生点 (x,y)、fuel 继承创造者的 fuel_limit
- 造物的 WAT 编译：wat::parse_bytes 失败 → 该次 spawn 作废+warn（不崩世界，参照玩家进场容错）
- 造物同样创建沙箱（EntitySandboxMap::create），从下一 tick 开始跑自己的代码——**造物是活实体，不是装饰**
- 返回值：1=已排队，0=拒绝（超限/编译失败在阶段二才知道的，日志为准）
- api_calls 元数据记录 "spawn"

### B. 新 import：destroy
`env.destroy(id_lo: i64, id_hi: i64) -> i32`（EntityId Uuid 传两半，或用 16 字节内存写入+ptr——实现自定，api.md 写清）
- 产权约束（宪法）：只能销毁 ①自己 spawn 的造物（产权链 owner==自己）②自己通过攻击击杀的——MVP 只实现①：world 里查该实体的 owner 是否等于调用者 owner；不匹配则拒绝+warn
- 通路同 A：pending_destroys → 阶段二统一应用（标记 alive=false 走死亡清理；destroy 不触发掠夺——不是击杀）
- 返回 1=已排队 0=拒绝

### C. 首个造物示例：哨塔（sentinel）
- `players/README.md` 或新文件 `examples/sentinel.wat`（**注意：放 examples/ 不放 players/，避免被当成玩家自动进场**）
- 哨塔代码：life()=true；on_tick 里 visible_count>0 时 log(2)（哨兵观察信号）——最简版哨塔（更聪明的留玩家自己写，README 引导）
- README/api.md 更新：哨塔=超视距情报的正解之一（#018），给出"玩家进场代码里 spawn 哨塔"的最小 WAT 片段示例

### D. 文档
- api.md：spawn/destroy 条目正式化（参数/限制/产权规则/示例片段），从"路线图"移入"已开放"（开放数 8→10）
- decisions.md：#020（门二开放定案：spawn±10格/64上限/产权链 owner；destroy 仅限自家造物 MVP）

## 验收标准

1. 新测试：spawn 排队→阶段二造物诞生（owner 继承/坐标/沙箱创建/下一 tick on_tick 执行）；超 ±10 格拒绝；超 64 上限拒绝；编译失败作废不崩；每 tick 1 个限流；destroy 自家造物成功（标记死亡+无掠夺）；destroy 他人造物拒绝
2. 457 基线零回归（四生物剧本时间线不变）
3. 冒烟：一局里 example 玩家的代码 spawn 出一个哨塔造物——日志可见"造物诞生"+哨塔 on_tick 心跳 log(2)。MVP 验证方式：进程内 tick 测试（等价可重复），报告贴摘录
4. clippy/check 三连过
5. api.md/decisions.md/README 同步

## 已知信息（不需要查）

- SandboxData 加字段模式参照 pending_attacks（Vec + drain）
- move_by 的 pending_moves 通路是 spawn 通路的模板（读 main.rs run_player_code 与 tick 阶段二）
- destroy 的 EntityId 传递：WAT 侧 i64 两个参数（uuid u128 拆两半）最省事；host 侧 EntityId(Uuid::from_u128(hi as u128 << 64 | lo as u128 & ...))——注意符号扩展陷阱，从 i64 转 u64 用 as
- 造物 birth 后与玩家同规则：生存点 decay、可被攻击、可下线无关（自治锚合同——产权链独立于创造者在线状态，创造者残影/消散不影响造物存续）
- fuel trap 坑（.cc-bugs #5）：测试 fuel 给足
- cargo 转发：node C:/Users/Administrator/AppData/Local/Temp/cc-cargo.mjs（不存在则重建）

## 硬 Gate

```bash
cd D:/projects/CodeWorld && cargo test --workspace
cd D:/projects/CodeWorld && cargo clippy --workspace --tests -- -D warnings
cd D:/projects/CodeWorld && cargo check --workspace
```

## 审查（轻装挡单审一轮）

聚焦：spawn/destroy 产权校验不可绕过（WAT 侧伪造坐标/ID 的边界）、阶段二应用顺序（spawn 后续 tick 才跑、destroy 与攻击死亡清理的交互）、限流逻辑、宪法符合性（destroy 无掠夺）。

## 交付物

1. 中文 commit（feat: 期二门二开放——spawn/destroy+产权链+哨塔示例）
2. C:/Users/Administrator/AppData/Local/Temp/cc-report.md（简版三节）

不要弹选项；信息缺失超3次试错即停写报告。
