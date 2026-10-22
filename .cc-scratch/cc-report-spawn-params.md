# cc-report — spawn 新参数批：出生数据包 blob + 出生重叠拒绝锚 + 出生初速（t_4b524940）

- **完成时间**：2026-10-21
- **commit**：`e9ce50a4a41ab5ff32dd328304a3b87d2811d0bd`（feat: spawn出生参数批——出生初速vx/vy(极速25严格>拒绝恰等放行)+出生数据包blob≤64KB(超限整次拒绝不裁剪)+birth_blob读回import(第13个,三态返回)+重叠拒绝贴边相切放行锚(14参破坏性变更同步pioneer/全部测试生成器/api.md)）
- **基线**：4291d24（开工时 HEAD）
- **TDD 全程**：红→绿 4 个循环（初速校验 → blob 校验/读取 → birth_blob import → process_spawns 落库），每轮先见红再写实现

## 改动文件清单（本 commit 7 文件 + 随兄弟 commit 入库 1 文件）

| 文件 | 改动 |
|---|---|
| `crates/arena/src/sandbox.rs` | MAX_SPAWN_BLOB_LEN=64KB 常数；PendingSpawn 增 vx/vy/blob 字段；SandboxData 增 birth_blob 字段；spawn import 升 14 参（move 闭包捕获 max_speed）+ 初速校验（有限+模长≤装配极速，严格>拒绝恰等放行）+ blob 长度预检（超限读内存前整次拒绝）+ 原样读取；新 import `env.birth_blob`（第 13 个，三态返回，原子写防半截，cap 负值回绕护栏）；测试生成器全升 14 参 + 新增 spawner_velocity_wat / spawner_blob_wat / birth_blob_reader_wat / read_memory 辅助；14 条新测试 |
| `bin/codeworld/src/main.rs` | process_spawns 造物诞生路径：entity.set_velocity((vx,vy)) 落库 + blob 交付造物沙箱 SandboxData.birth_blob + 诞生 info 日志增 vx/vy/blob_len；birth_motion_spawner_wat / once_spawner_wat 升 14 参；直构 PendingSpawn 用例补 3 字段；3 条新测试 |
| `players/pioneer.wat` | import 签名 14 参 + 立塔调用补 4 实参（零初速/无包，行为不变）；CRLF 保持 |
| `players/pioneer.wat.txt` | 与 pioneer.wat 逐字节一致（md5 同 0176502b），CRLF 保持 |
| `docs/api.md` | import 计数 12→13；spawn 签名/参数表 +4 行（vx/vy/blob_ptr/blob_len 含语义与生效范围注记）/作废条款补初速与 blob 超限/示例 WAT 补 4 实参；新增 `birth_blob` 节（三态返回表+生命周期+示例） |
| `docs/game-menu.md` | 1.7 常数总账出生参数域行：补「初速≤25 / 出生数据包≤64KB（超限拒绝不裁剪）」，将来成员收敛为形状掩码，已落地三项注明 |
| `docs/progress.md` | 「已完成」顶部追加 1 行收工记录（含 17 新测试与计数） |
| `docs/dev-plan.md` | D-018 工程清单②项改注「初速已提前落地为第 11/12 参（D-003 motion 占第 10 参），剩余=③通用有速度实体物理」——**注意：此文件改动随兄弟 commit `13088d2` 入库**（兄弟整文件提交时我的 hunk 已在工作区，内容已在库，未丢） |

## 测试结果

- **全 workspace：894-897 通过 / 0 失败**（数字随兄弟在途编辑波动，本刀收工时点 894 全绿；核心三 crate：arena 104 / core 188 / codeworld 377 均全绿）
- **本刀新增 17 测**（负面路径 6，满足 ≥12/≥6）：
  - arena 14：`spawn_import_rejects_birth_velocity_over_max`（负）/`spawn_import_allows_birth_velocity_exactly_max`（恰等边界）/`spawn_import_rejects_nonfinite_birth_velocity`（负）/`spawn_import_passes_birth_velocity_through` /`spawn_import_reads_birth_blob_from_memory` /`spawn_import_empty_blob_len_zero_is_legal` /`spawn_import_negative_blob_len_is_empty_packet` /`spawn_import_rejects_oversized_blob`（负）/`spawn_import_allows_blob_exactly_64k`（恰等边界）/`birth_blob_import_returns_zero_when_no_packet` /`birth_blob_import_writes_and_returns_len` /`birth_blob_import_returns_negative_when_cap_short`（负）/`birth_blob_import_returns_negative_when_cap_zero`（负）/`birth_blob_import_returns_negative_when_ptr_illegal`（负）
  - main 3：`process_spawns_writes_birth_velocity_into_creation` /`process_spawns_delivers_birth_blob_end_to_end`（端到端字节一致）/`process_spawns_allows_adjacent_edge_touching_birth`（贴边相切放行边界）
- **已知红说明**：任务书指名的 D-003 预存红 `process_spawns_dynamic_motion_birth_precheck_uses_circle` 在本刀施工期间已被 D-003 worker 自行修复（现绿），未由我修复。收工时点 workspace 0 失败。
- **grep 自查**：全仓 12 处 spawn import 声明全部 14 参（i32 i32 f64 f64 i32 i32 f64 f64 i32 i32 f64 f64 i32 i32），零旧签名残留（.cc-scratch 排除）。

## 硬 Gate 结论

| Gate 项 | 结果 |
|---|---|
| `cargo test -p codeworld-arena` | ✅ 104 全绿 |
| `cargo test -p codeworld-core` | ✅ 188 全绿 |
| `cargo test -p codeworld` | ✅ 377 全绿 |
| `cargo check --workspace` | ✅ 过（有 2 条 warning，均属兄弟在途 hunks，见下） |
| `cargo test --workspace` | ✅ 0 失败（894-897 过，数字随兄弟编辑波动） |
| `cargo clippy -p codeworld-arena -p codeworld -- -D warnings` | ⚠️ **被兄弟在途 lint 卡住**（见下） |

### clippy 卡点详情（非本刀 hunks，铁律禁修）

- `crates/core/src/entity.rs:175` doc 注释缩进 lint（clippy::doc_lazy_continuation）——D-003 worker 的**未提交** hunks（git show HEAD 无此行，工作区 diff 所有）。该文件不在本刀清单，按并行铁律「其他文件问题不修不动」未动。
- `bin/codeworld/src/main.rs:2860` unused variable `world`——D-007 遗嘱批的未提交 hunks（本 commit 里同文件的其他 hunks 是他们的在途代码）。同上未动。
- **本刀自身代码 clippy 干净**：对 sandbox.rs / main.rs 本刀 hunks 范围 grep clippy 输出，0 告警 attributable。
- 症状：clippy -p codeworld-arena -p codeworld 经传递依赖 codeworld-core 编译，被上述两处 -D warnings 变 error 卡死。等兄弟收口自愈即可绿，无需本刀动作。

## 自审查轮次结论

<!-- REVIEW_RESULTS -->

## 语义要点备忘（审阅 commit 时先读这段）

- **出生初速生效范围**（如实写进 api.md）：velocity 字段落库；仅 dynamic 造物有物理意义；**位置积分（被初速带着飞）未做**——D-018「通用有速度实体物理」的活。static 声明初速合法照存。
- **blob 不解释内容**（D-017 挂靠立法）：世界只搬运字节；blob_len≤0=空包合法；超限**整次拒绝不裁剪**（裁剪=偷改玩家数据），且在读内存前拦下防大拷贝。
- **birth_blob 三态**：0=无包（玩家进场/弹丸恒此路径）；正 N=原子写入；负 -N=容量不足或指针非法未写一字节（日志区分两种原因）。生命周期随沙箱，跨重启不保留。
- **恰等极速放行**：与 shoot 完全同口径（严格 > 判据），极速值经 SandboxManager::new_with_max_speed 装配进 spawn 闭包（改 move 闭包捕获），非硬编码 25。

## 附录一：gstack 注入信息

- **改动文件**：见「改动文件清单」。
- **技术决策**：①spawn 闭包从非 move 改 move 捕获 max_speed（与 shoot 同源装配值，不硬编码 25）；②blob 读取复用 read_payload_from_caller（ptr<0/越界返回空 Vec=空包合法，任务书指定的现成语义）；③birth_blob 写回复用 write_bytes_to_caller 的整段校验语义（原子：能整体写入才写），cap 负值先判符号再转 usize（防回绕巨数）；④adjacent 边界锚的几何：creation_aabb(12,0)=[12,13]×[0,1] 格对齐，占据者圆 r=0.5 圆心挪半格 (13.5,0.5) 距格边恰 0.5=相切（严格 < 不重叠）→诞生，与「占据格=拒绝」构成边界两侧双锚。
- **踩坑**：①`Option::unwrap()` 首跑红是测试自己漏建创造者沙箱（非实现 bug）；②AABB 首版测试几何算错（以为 AABB 是 [11.5,12.5]，实际格对齐 [12,13]），红因是测试预期错而非实现错——按 TDD 修测试断言对象；③碰上 phantom 编译错（core 符号时有时无）=兄弟并发构建污染 target 指纹，touch lib.rs 强制重编解决。
- **项目级约束**：并行 7 worker 同工作树，文件级 commit 必然携带共享文件里兄弟的在途 hunks（main.rs 外来 hunks 约 600 行：D-007 遗嘱/接触块 + D-017 attacks 快照 + D-003 测试改写；api.md 含 D-003 motion 文档 hunks）——**commit 里的代码 = 全量测试绿的工作区状态**，这正是"测试通过才能 commit"能保证的最大一致面。

## 附录二：失败路径（试了不管用的方案）

- hunk 级提交（git apply --cached 只挑本刀 hunks）：评估后放弃——main.rs 测试区 hunk 与 D-003 的测试改写在上下文行上纠缠，手挑会提交一个**从未被测试过的混合态**，违反"测试通过才能 commit"的实质；文件级提交的恰是 897 测试覆盖的状态。
- 等 clippy 自愈后再交：等待期间兄弟持续提交/改写（4 次 clippy 重试跨约 40 分钟未自愈），阻塞在本刀不可控的外部状态上；按并行铁律选择披露而非代修。
- dev-plan.md 独立入库：兄弟 commit 13088d2 抢先把整文件（含我的 D-018 hunk）提交，我的 commit 无法再包含它——内容已在库，仅 commit 归属变化，报告披露。

## 附录三：经验教训（非显而易见的问题）

- **`Entity::new` 默认 radius=0.5 且 creation 身体是格对齐 AABB（floor 到格）**，不是以 pos 为中心的 ±0.5——算相邻/重叠边界时必须用格对齐几何，直觉的"半径外扩"会算错半格。
- **TDD 里"守卫型测试"先行合法**：恰等放行/透传类用例在实现前就绿（防过严回归），红证据由拒绝类用例承担；blob 实现曾顺手先写，按纪律回退重走红→绿（先码后测=证明不了测试能抓 bug）。
- **并发 cargo 会产出 phantom 编译错**：文件 md5 稳定但 `cargo test -p codeworld` 与 `cargo build -p codeworld-arena` 结论相反，touch 依赖的 lib.rs 强制重编即愈——先怀疑 target 指纹再怀疑代码。
- **WAT 测 64KB blob 边界不用真备数据**：内存零初始化，blob_ptr=0 + blob_len=65536 直接读零页；超限拒绝发生在读内存前，传 i32.const 65537 即可测。
- **进程内 e2e 读造物 memory**：`handle.instance.get_memory(&mut handle.store, "memory")` 直接断言字节，比让造物 log 中转或全局聚合更硬。

## 附录四：环境变化

- 无构建/测试环境变化。cargo 全路径 `C:/Users/Administrator/.cargo/bin/cargo.exe` 全程可用；测试基线从开工时 81(arena)/356+1红(codeworld) 演进到收工时 104/377 全绿（增量含兄弟并行落地的测试）。
- target/ 目录出现一次并发构建指纹污染（见附录三），touch 后自愈，无持久影响。
