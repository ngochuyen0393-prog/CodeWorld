# D-018 弹丸退役工程——施工报告

> 任务书：TASK-d018-retirement.md（宪法级大刀，decisions #045 执行刀）。
> 施工方式：**git worktree 隔离**（.cc-wt-d018，HEAD=1a69d51 干净基线）——主树有 D-009 液体刀 WIP 在途且编译被其 mut 笔误阻塞，无法建基线；按 D-016 R84 "提交树隔离验证法"先例执行。worktree 施工+全量 Gate 后落回 master。

## 一、改动文件清单（14 个，逐个列出）

**crates/core：**
1. `crates/core/src/collision.rs` — PROJECTILE_CODE/PROJECTILE_RADIUS/is_projective 注释更新为"历史实体兼容判定"（常量保留：兼容判定+操控定装弹/教学模板同款小弹数值锚；G1 不 grep 它们）

**crates/physics：**
2. `crates/physics/src/default.rs` — 核心工程②：删 PROJECTILE_MAX_AGE_TICKS/ProjectileEvent/step_projectiles 整函数；step 冲量路径加 CCD 扫掠截断（撞体/触地→位移截断+速度清零；停点 1cm 推进修接触浮点缝隙——首触点恰在 ≤ 边界上浮点向上漂即漏，推进后停点窗口内 1cm 当拍触发接触）；删 is_proj 跳过分支（旧弹丸实体走通用路径）；新增 swept_first_body_contact 辅助；测试区同态改写（弹丸构造→dynamic 造物+初速；积分器换中点口径重标：射程锚 3.9→≈7.89、抛物线数字、CCD 停点）+新增造物弹测试；积分器双轨注记入码

**crates/arena：**
3. `crates/arena/src/sandbox.rs` — 删 PendingShoot/pending_shoots/shots_fired/TraceProbe/traces 字段/SHOOT_REACH_* 常数/seg_point_distance/shoot_reach_min_distance/shoot import 注册段（留退役注记）/probe 痕迹分支（-3）/new_with_reach_physics（drag 装配随 shoot 退役）；测试区删 21 个 shoot/痕迹测试；注释口径批量修正（"与 shoot 同源"→"与飞行钳制同源"等 8 处）

**bin/codeworld：**
4. `bin/codeworld/src/main.rs` — 删 LaunchTrace/TRACE_STRONG_KE/traces/expire_traces/bridge_traces 组装/collect_pending_shoots/projectiles 账本+ProjectileMeta/process_shoots 全家/settle_projectile_events/弹丸账本绑定清算段（造物通用段保留）；**操控射击改 spawn**（ctrl_shell_spawns: Vec<PendingSpawn>，出生点外推 0.75 格，a1 步同一 process_spawn_requests 出口）；**H 改投掷器**（hunter_wat() 生成器：子弹代码内嵌 data[4096]、vx=(dx-0.75)/1.805 中点口径系数、vy=+9.53）；**遗嘱烟花改 spawn 造物带初速**（dying_will_firework_wat）；run_death_wills 单返回 Vec<PendingSpawn>；CTRL_SHELL_WAT 弹头模板（无 on_tick/on_contact 出刀/$age 延迟 2 拍消亡——结算需攻击者在册）；solve_launch_vector simulate_path 同步中点口径（#028 镜像铁律：世界物理变了镜像必须跟，定装参数档位全不动）+CTRL_SOLVE_MAX_TICKS 局部常数；ctrl_shoots→ctrl_shell_spawns；测试区改写 20+ 测试（smoke 证据链改 4 拍时序/ws_shoot 造物弹断言/hunter 锚同态保留/grace 造物弹版/projectile_flies 新语义：弹=独立攻击者）；删 3 个 shoot 专属测试
5. `bin/codeworld/src/net.rs` — entity_kind PROJECTILE_CODE 分支加历史兼容注释（交付物⑤）

**文档：**
6. `docs/api.md` — shoot 词条→退役说明段（G2 ✓ 剩余 4 处提及=退役段+历史锚）；visible_count 痕迹句删；probe -3 段删（历史注记保留）；on_contact/遗嘱/贴身刀/field_pulse 五处措辞同步
7. `docs/game-menu.md` — 三条 🪦→✅ 已退役划线（2026-10-22 D-018 执行）+出生参数域行补 D-018 执行完成（G8 ✓）
8. `docs/dev-plan.md` — D-018 标题加 [✅ 已落地]（commit hash 收尾补）
9. `docs/newbie-first-thrower.md` — **新建**：投掷器模板第一课（G7 ✓ 五段：完整可抄模板/组装三步/发射点外推/常见坑 6 条/历史注记）
10. `docs/progress.md` — 现在态登记

**players：**
11. `players/me.txt` — 末尾补退役注记一行（untracked 文件，主树直改不进 commit）

**.cc-scratch（工作产物不入 commit）：**
12. `.cc-scratch/cc-report-d018.md`（本报告）
13. `.cc-scratch/d018-*.js/.log/.txt`（施工/诊断脚本与证据日志）

**主树（本刀 hunks 落回时同走）：** docs/progress.md 在 D-009 WIP 之外属共享登记文件。

## 二、测试（G3）

- **最终全量：workspace 18 目标 1034 passed / 0 failed**（post-register 复跑同数字）
- 基线 1069 → 1034：**净 -35** = 删 37 个 shoot 专属测试（arena 21+main 16，任务书⑥明确授权"import 语义（删）"）+ 新增 2 个通用路径锚（ballistic_shell 系列 + is_creation_shell 判定族）。**零非授权测试损坏**——所有保留测试全绿。
- 交付物⑥三问执行记录：import 语义测试（shoot_import_* 15 个）全删；弹道物理测试同态改写（构造从 push_shoot → dynamic 造物+set_velocity，走 step，断言语义保留、数字按中点口径重标）；战术行为测试改 spawn 发射后断言不变（hunter 两拍回弧命中锚同态保留）。

## 三、冒烟（G5/G6 测试名+结果）

- **G5 H 弹道回归**：`hunter_shoots_projectile_at_eighth_tick_and_hits_static_victim` ✅ PASS——H 每 8 拍投掷、t9 两拍回弧落靶身接触、t10 结算击杀；弹道系数实测换算（中点口径）：vx=(dx-0.75)/1.805、vy=+9.53（mass 0.5，两拍 y 位移和=1.805·vy-17.199≈0）
- **G6 操控射击**：`ws_shoot_intent_spawns_projectile_with_fixed_loadout` ✅ / `ws_shoot_intent_unreachable_target_spawns_no_projectile` ✅ / `ws_shoot_rate_limited_to_one_per_tick` ✅ / `ws_shoot_and_move_coexist_across_ticks` ✅——速度梯（12 基准/1.15 补偿/22 上限）不动、每拍 1 次（spawn 折叠承担）、边走边射语义不变；解算器 simulate_path 同步中点口径（#028 镜像铁律——世界物理变了镜像必须跟，否则"解算说能打中=世界真命中"契约断裂；**任务书偏差决策**：⑤"解算器不动"指定装参数档位，积分器镜像同步是物理换轨的必然）
- 附带冒烟：`smoke_ballistics_launch_to_intercept_evidence_chain` ✅（4 拍全生命周期：出生→接触→结算拦截→消亡）；`will_spawn_firework_same_tick` ✅（遗嘱烟花=spawn 造物带初速）

## 四、Gate G1-G8 逐条证据

| Gate | 命令 | 结果 |
|---|---|---|
| G1 | `git grep -n 'env" "shoot"\|PendingShoot\|step_projectiles\|LaunchTrace\|PROJECTILE_MAX_AGE' -- crates/ bin/codeworld/src/` | shoot import 0 结果；PendingShoot/step_projectiles/PROJECTILE_MAX_AGE 仅存于**退役注记注释**（历史可查证性，无一处代码符号） |
| G2 | `grep -c shoot docs/api.md` | 4 处=退役说明段（L153/155）+历史锚（L49 遗嘱/L288 field_pulse 退役关系叙述）✓ |
| G3 | `cargo test --workspace` | **1034 passed / 0 failed**（18 目标）；基线对照见上节 |
| G4 | `cargo clippy --workspace --all-targets` | 本刀 hunks **零警告**；余 4 条（mask.rs:448/avatars.rs:501/net.rs:1184/main.rs:7477）全是 HEAD 既有代码被新 clippy lint 标出（manual !Range/into_owned），非本刀 hunks 不动（不踩 D-017 文件） |
| G5 | hunter 锚测试 | ✅（见冒烟节） |
| G6 | ws_shoot 三测试+边走边射 | ✅（见冒烟节） |
| G7 | 投掷器模板文档 | `docs/newbie-first-thrower.md` 五段完整（模板可抄/三步/外推/6 坑/历史） |
| G8 | game-menu 三条+api.md | ✅ 三条 ✅ 划线+出生参数域注记；api.md 同步完成 |

## 五、任务书偏差决策记录（3 项，报告显著声明）

1. **解算器镜像同步中点口径**（⑤"解算器保留不动"的字面违背）：定装参数（基准 12/补偿 1.15/速度梯/容差 0.65）全部不动=操控手感档位不变；但 simulate_path 的积分器必须跟世界物理（#028 镜像铁律）——弹丸半隐式管线退役后，真实弹（造物）走 step 中点位移，镜像不同步=预测全 miss。手算验证：中点位移=匀加速解析解，同参数弹道比半隐式远（重力响应提前半拍），旧镜像下 15 格目标预测命中实际偏 10+ 格全 miss。
2. **H 弹道参数换算**（⑥"参数照抄"的字面违背，守"手感必须不变"的实质）：vx=dx/1.71/vy=14.7 是半隐式口径实测值；中点口径下照抄=弹飞过头 4.7 格 miss。换算推导入 hunter_wat 注释（两拍 y 位移和=1.805·vy-17.199≈0 → vy=9.53；x 同理 1.805 系数），并扣除外推 0.75。G5 锚（两拍回弧命中静态靶）同态保留 ✅。
3. **弹头延迟 2 拍消亡**（任务书未预见的时序事实）：接触拍 on_contact 排刀、次拍 a2 结算需要攻击者在册——立即死=死者刀作废白打。$age 计数在 life() 里递增、>=2 返 0（接触→出刀→结算→消亡完整生命周期）。附带发现：CCD 停点恰在 detect_touching ≤ 边界上浮点即漏——1cm 推进修缝（physics 代码注释已锚定）。
4. （流程）**worktree 隔离施工**：任务书"master 直改"在 D-009 WIP 污染主树（编译被兄弟 mut 笔误阻塞）前提下不可行，按 D-016 R84 先例执行 worktree 隔离+落回。

## 六、交接注记（给 D-009 卡/收尾刀）

主树 D-009 液体刀在途（spawn 17→19 参方向）。其落地时需同步本刀新增的 3 处 spawn 调用点补 2 个液体参：
- `hunter_wat()` / `shooter_once_wat()` / `dying_will_firework_wat()` 的 WAT spawn 调用（main.rs，加 2 个 f64 压栈）
- 操控射击 `ctrl_shell_spawns` 的 PendingSpawn 构造（main.rs Shoot 分支，加 liquid 字段）
- CTRL_SHELL_WAT 弹头本身无 spawn 调用不受影响

## 七、待土豆人工验收清单

1. `start.bat` 起服 watch 日志：H 搅局者每 8 拍 `[spawn] 造物请求已排队` → 造物弹诞生 → 接触 → 击杀掠夺（弹道手感=两拍回弧）
2. 浏览器操控射击：点远处敌人 → 定装弹（小圆点 creation_dynamic）飞出命中
3. 遗嘱烟花：带 on_death spawn 的生物死亡 → 造物弹当拍出生带初速
4. 弹幕躺尸：命中后未写自杀代码的弹停驻地面（占实体名额）——#045 认账的上限 64 护栏兜底；"寿命管理第二课"候选


## 八、Rebase 收口（关键变更：D-009/D-004b 兄弟卡中途落库）

施工中段，master 前进到 162fd82（D-009 水体 19 参 spawn+液体物理+step_projectiles_with_liquids 液体耦合；D-004b 渲染）。落回改为 **rebase --onto 162fd82** 手工解冲突：

1. **液体物理迁入 step 冲量路径**：D-009 在已退役的 step_projectiles_with_liquids 里的液体积分（v←v×k_eff+g_final 替代重力入速度、m_eff 进阻力分母、零速入水速度制路由）原样迁入 step_with_liquids 冲量路径——D-018 通用化与 D-009 液体正交叠加，全部 D-009 液体测试照绿。
2. **spawn 19 参同步**：hunter_wat/shooter_once_wat/dying_will_firework_wat 三 WAT 签名补 2 个液体 f64 + 压栈补参；ctrl_shell_spawns/grace 测试两处 PendingSpawn 构造补 liquid_density/liquid_drag 字段。
3. **CCD 接近性判据（物理修正）**：segment_first_contact_circle 起点重叠分支从无条件 t=0 改为"接近中（f·d≤0）t=0 / 严格分离中放行"——D-009 木浮铁沉测试实测暴露：浮体上浮穿过沉体被无条件截断清零速度，浮沉涌现消失。横穿圆内/零位移仍 t=0（D-009 两锚测试照绿）。
4. **液体造物不作 CCD 目标**：swept 目标过滤加 liquid_density>0 排除——泡在水里不撞水本身（浮沉体穿越水域布景；实体撞实体仍走 detect_touching/接触通道）。
5. **木浮铁沉布景适配**：两球深重叠出生（D-009 布景）在新 CCD 语义下=固体堆叠（浮体被沉体压住=正确物理），水域加宽为两块 pond（[30,32]），两球错位出生独立浮沉。

**Rebase 后终验（主树 614e8a0）**：workspace 1074 绿 / 0 failed；本刀 hunks clippy 零警告（余 6 条全为 D-009/D-017 既有代码的新 lint，非本刀 hunks）。

**主树落回方式**：worktree rebase commit 9cee29b（基于 162fd82）→ master --ff-only → docs hash 补登 614e8a0。