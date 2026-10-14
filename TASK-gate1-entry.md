# TASK（轻装挡）：期一·门一进场——players/ 目录 + 在线锚 + 战斗残影期 + api.md

## ⚠️ 必须使用 test-driven-development（红→绿）

## 宪法依据（不可违反）

docs/world-foundation.md（一个基座两扇门三张合同）+ decisions.md #011-#015。核心：能力归框架，玩法归玩家；战斗残影期不可豁免。

## 项目上下文

项目：D:/projects/CodeWorld（Rust workspace，基线 426 测试全绿，git 4d48e75）
现有进场：spawn_test_entities 硬编码 A/D/H/S 四生物（main.rs，含开关 CODEWORLD_H_ENABLED / CODEWORLD_S_ENABLED / CODEWORLD_D_SHIELD）
实体结构：Entity::new(EntityId, PlayerId, code_hash, pos, Appearance::new(glyph,color,size), tick, 指令配额)；沙箱 EntitySandboxMap::create(entity_id, wasm_bytes, fuel)
tick 流程：阶段一玩家代码（快照填充+执行）→ 阶段二（应用pending→收件→结算→decay→物理→死亡清理→渲染→快照）
cargo：PATH 无 cargo 时 node C:/Users/Administrator/AppData/Local/Temp/cc-cargo.mjs 转发（不存在则重建）

## 需求（四块）

### A. players/ 目录进场（门一）
1. 世界启动时扫描 `players/` 目录：每个 `.wat` 文件 = 一名玩家
2. 文件名（去扩展名）= 玩家名 = code_hash 字段值；glyph 取玩家名首字符大写；颜色从固定色轮（red/cyan/yellow/purple/green/magenta/blue…）按加载顺序轮转分配（A红/D青/H黄/S紫已占用，玩家从 green 起轮）
3. 出生点：x 在 [30, 45] 随机（用 EntityId UUID 做种子或简单 hash，避免引入 rand 依赖也可以顺序分配 [30,35,40,45] 循环）、y=0
4. .wat 文件内容 = 玩家代码文本，用现有 wasm()（wat::parse_bytes）编译；编译失败：warn 日志 + 跳过该玩家（不崩世界）
5. players/ 目录不存在时自动创建（空目录=无玩家进场，原四生物剧本照跑）
6. 项目自带示例：写一个 `players/example.wat`（最简玩家：life()=true + on_tick 调 log(1)），作为"第一个玩家模板"，README 一句话说明
7. 配置：CODEWORLD_PLAYERS_DIR 环境变量可覆盖目录路径，默认 ./players

### B. 在线锚合同（文件=在线信号）
1. 世界运行中每 tick 检查 players/ 目录：新出现的 .wat = 新玩家进场（读文件加载，同 A 规则）；被删除的 .wat 文件对应的玩家 = 下线
2. 下线流程：角色实体进入"残影状态"——标记后仍存在于世界 5 拍（可被攻击/掠夺/移动惰性：残影期不执行玩家代码，即沙箱不再调 on_tick/life，但实体还在 world 里能挨打）
3. 5 拍后残影消散：实体移除+沙箱销毁（走现有死亡清理通路，但不算"击杀"——无掠夺对象，只是消散）
4. 文件内容变化（同名重写）：视为"下线+上线"？MVP 不处理内容热更新——同名文件只在首次进场时读内容，重写内容需删了再放（README 注明）。日志记清楚

### C. 战斗残影期（#013 铁律，防拔线逃课）
1. 记录每个玩家角色实体"最近参与战斗的 tick"（参与=作为 attacker 或 target 出现在任何 PendingAttack 的收件/结算中）
2. 下线触发时检查：若 now - 最近参战 tick < 30，残影期不是 5 拍而是 max(5, 30 - (now-参战tick)) 拍——即"战斗状态下线，至少留到脱离战斗窗口"
3. 残影期可被正常攻击/击杀（击杀则走正常掠夺，残影身份不影响掠夺合法性）
4. 日志：下线时打 info（玩家名/是否战斗残影/残影拍数）

### D. api.md 接口宪法 v1
1. 写 `docs/api.md`：面向玩家的世界能力接口文档
2. 内容：(1) 实体导出函数契约：life()必需/on_tick/on_probe/on_attack 各自语义 (2) 已开放 8 个 import（log/get_pos/scan/move_by/attack_at/visible_count/visible_at/probe）逐个：参数/返回/限制（如 payload 1MB 上限、move_by 拒绝条件、probe 坐标寻址+体徵尾块）(3) 进场说明：players/ 目录用法、命名规则、颜色/出生点规则、下线语义（残影）(4) 三态合同简表（指向 world-foundation.md）(5) 攻击语义铁律四条 (#001) 摘录 (6) 路线图：门二 spawn/连接锚 register_endpoint 将来开放
3. 语言：中文，面向"会写代码的玩家"，每接口给最小 WAT 示例片段
4. 从 api.md 内容反推：players/example.wat 必须与文档示例一致

## 验收标准

1. players/ 放入 example.wat 启动世界：日志出现"玩家进场：EXAMPLE"（名字/glyph/颜色/出生点），该实体与四生物同场，on_tick 正常执行（log(1) 可见）
2. 运行中删除 .wat：日志"玩家下线"+残影计时；5 拍后（或战斗残影更长）实体消散，无残留
3. 战斗残影：玩家被攻击后 30 拍内删文件 → 残影期 >5 拍（测试断言具体拍数）；残影期内被击杀 → 掠夺正常发生
4. 编译失败的 .wat：warn 日志+跳过，世界不崩
5. 新测试覆盖：目录加载/在线检测/残影时长（普通5拍/战斗延长）/残影可被击杀掠夺/编译失败容错。426 基线零回归
6. docs/api.md 存在且含上述 6 节；players/example.wat 与文档示例一致
7. clippy/check 三连过
8. 冒烟：完整跑一局含"进场→删除→残影→消散"的剧本，日志摘录进报告

## 已知信息（不需要查）

- Entity 无 name 字段，code_hash 是身份承载（A/D/H/S 模式相同）
- 死亡清理通路在 tick 阶段二（收集 dead_ids → sandbox_map.remove → retain）
- 残影"不执行玩家代码"：run_player_code 跳过残影实体即可（类似无沙箱实体的跳过路径，但残影实体有沙箱——别销毁，留到消散）
- 攻击参与记录：deliver_attacks/settle_attacks 里 attacker 与 target 的 tick 均需记录
- 文件监控用 std::fs 每 tick 读目录即可（MVP 轮询，不引 notify 库）
- wat 编译失败已有先例处理模式（replace_sandbox 先建后拆）
- UI 无改动；渲染层自动画新实体（glyph/color 走 Appearance）

## 硬 Gate

```bash
cd D:/projects/CodeWorld && cargo test --workspace
cd D:/projects/CodeWorld && cargo clippy --workspace --tests -- -D warnings
cd D:/projects/CodeWorld && cargo check --workspace
```

## 审查（轻装挡：会话内 Agent 子代理单审一轮）

聚焦：残影状态机正确性（下线→残影→消散/被杀的路径全覆盖）、战斗残影计算、目录轮询的资源开销、宪法符合性（不越界做玩法功能）。无 P0/P1 即收工。

## 交付物

1. 中文 commit（feat: 期一门一进场——players目录+在线锚+战斗残影期+api.md）
2. C:/Users/Administrator/AppData/Local/Temp/cc-report.md（简版三节：改动/测试/冒烟摘录）

不要弹选项让用户选；信息缺失超 3 次试错即停写报告。
