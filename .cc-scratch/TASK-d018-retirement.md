# TASK: D-018 弹丸退役工程——shoot 移除/弹道通用化/痕迹退役/三常数移除/D-H-WAT 迁移/投掷器模板第一课

[Superpowers 激活：verification-before-completion 文档级。]

你是 CodeWorld 项目（D:/projects/CodeWorld）的施工工程师。本任务书是宪法级大刀 **D-018 标准弹丸退役工程**（decisions.md #045 已立法，dev-plan.md D-018 节工程清单），前置 D-012 力场系统已全部落地并验收（2026-10-22）。动手前通读 dev-plan.md 的 D-018 节与 decisions.md #045/#052。

## 背景与宪法依据（为什么拆）

#045 裁决：shoot 制式弹丸（0.15 圆点+世界弹道管线）从世界 API 退役——世界配发制式弹=世界造车，违反 #038 修路不跑车/#011 零预设/#041 三位一体。shoot 转位**新手文档教学模板**（"第一课：拼一个投掷器"）。替代链零件已全部就位：spawn 投射物造物（出生初速已落地）+ field_pulse（分离式发射器）/thrust（自推）+ 通用物理（重力/阻力/CCD 从弹丸专属通用化为一切有速度实体）+ 接触通道代码攻（on_contact 边沿触发已有）+ 寿命自管（代码自杀/燃料尽）。

退役纪律（用户定则）：**先修新路再拆旧桥**——D-012 已落地即新路已通，本刀拆桥。退役不删除：shoot 相关代码降级为玩家侧教学模板。

## 交付物清单（七件）

### ① env.shoot import 移除（世界侧）

- crates/arena/src/sandbox.rs：删除 shoot import 注册（约 1406 行 PendingShoot push 处、4776 行注释段）与 PendingShoot 结构体、pending_shoots 字段。
- bin/codeworld/src/main.rs：删除 process_shoots/process_shoot_requests（2771-2862 一带）、collect_pending_shoots（820）、ctrl_shoots 字段及其收集（139/207/358/826/1655-1700 操控射击段）、遗嘱 shoot 通道（3500 run_death_wills 返回值只留 PendingSpawn）。
- 调用点同步：1823/1925-1928 的调用删除；shooters 映射构造（1828-1833）删除。
- **WAT 内嵌测试同步改写**：所有 `(import "env" "shoot" ...)` 的测试 WAT（3889/8629/11266/11584 等）改成等价的 spawn 发射（一体式：spawn code+vx/vy 初速）或直接删除不再测的 shoot 专属行为（痕迹/寿命/限流）。迁移原则见交付物⑥。
- **api.md shoot 词条退役**：第 155 行 shoot 词条整段改为退役说明（一段话：#045 退役+指向投掷器模板第一课文档），保留历史锚（防老玩家文档跳空）。

### ② 通用"有速度实体"物理——step_projectiles 并入 step（管线通用化）

核心工程。crates/physics/src/default.rs 的 step_projectiles（215-460）是弹丸专属管线（重力→阻力→极速钳→半隐式欧拉→CCD 扫掠命中→地面拦截），step（462+）已有 D-010a 冲量路径（生物/dynamic 造物速度积分：重力入速度→阻力→极速钳→**中点位移**→落地清零）但**无 CCD**。

通用化方案（保持两测试锚不变）：
- step 的冲量路径（me_vel != 0 分支）升级为完整弹道物理：位移前加 **CCD 扫掠命中检测**（复用 step_projectiles 现有 swept 逻辑：多目标 EntityId 定序取线段参数 t 最小者+地面拦截 terrain.segment_first_contact），命中身体/地面 → 产出事件供主程序结算。
- **积分器差异保留**（这是测试锚不是偷懒）：step_projectiles 半隐式欧拉（重力后速度直接位移）；step 冲量路径中点位移（v0=7.7 起跳 2.85 格锚 thrust_ballistic_jump_peak_within_three_grid_anchor）。合并方案：CCD 扫掠段复用，积分器**保留双轨**——原弹丸类实体与有速度造物在数字上会差半拍（起点不同历史锚定），通用化以"同一 CCD/阻力/极速/地面代码路径"为准，积分器差异在代码注释中写明原因（历史测试锚，防止将来误统一）。
- ProjectileEvent 改造：Landed/Expired/HitsBody 事件语义保留但改为通用 VelocityEvent（谁产生：任何有速度实体；消费：主程序既有结算管线）。**旧 PROJECTILE_CODE 实体退役后不再产生**——事件只由有速度实体产生（velocity≠0 的一拍）。
- **新实体语义**：一体式发射造物（spawn+初速）由既有 process_spawns 落 velocity 字段（已落地），本刀只补齐它在 step 里的物理待遇（现在它走冲量路径但无 CCD——高速弹丸造物会隧穿）。**CCD 命中后的事件→主程序如何结算**：复用弹丸命中现有通路（PendingAttack contact_exempt 或等价），保持"物理接触=规则层投递"的时序契约。
- step_projectiles 函数删除；其专属测试迁移为通用路径测试（同态改写：entity 用 dynamic 造物+初速构造，断言不变）。
- **fields 消费不变**：effective_gravity_drag 已在 step 内，场对一切有速度实体生效（D-012 既有行为保持）。

### ③ 痕迹机制退役

LaunchTrace 结构（main.rs 51-70）、traces 字段/expire_traces（127-228）、trace 推入（2844）、TraceProbe 桥（sandbox.rs 内 46 处引用一并清）。理由（#045 连带退役）：造物天然在 visible 列表天然暴露，独立痕迹机制多余。probe 返回发射者 id 的能力若被 test 依赖，迁移为造物时代等价断言（造物可见=天然暴露）或删除。

### ④ 三常数移除+总账同步

- crates/physics/src/default.rs:31 PROJECTILE_MAX_AGE_TICKS 删除。
- 弹丸半径 0.15：main.rs process_shoots 内的 0.15 硬编码随函数删除；若 Entity 构造默认处有残留一并清。
- 每拍 1 发限流：sandbox.rs shoot import 段的限流代码随 import 删除（spawn 自带每拍 1 次限流已覆盖）。
- **docs/game-menu.md 总账同步**：第 88/89/90 行三条 🪦 行改 ✅ 已退役划线（2026-10-22 D-018 执行）；出生参数域行（93）"已落地"注记补 D-018 执行完成。
- **docs/api.md 常数表同步**：射速限流/弹丸寿命相关行随 import 退役标注。

### ⑤ D/H/WAT 战术代码迁移（造物弹）

- **HUNTER_WAT（H 搅局者，main.rs 3883-3935）**：shoot 调用改为 spawn 一体式发射。参数翻译：原 shoot(vx=dx/1.71, vy=+14.7, mass=0.5) → spawn(子弹代码, x, y, glyph, color, radius≈0.15 同款小弹, mass=0.5, binding=0, motion=1 dynamic, vx=dx/1.71, vy=+14.7)。子弹代码=极简 WAT（无 on_tick 即死代码——出生即有速度飞出，撞地/撞人由物理+接触通道结算，燃料继承制兜底）。**H 的弹道参数照抄**（dx/1.71 与 +14.7 两拍回弧系数是实测锚定的，迁移后 H 的弹道手感必须不变——验收锚）。
- **遗嘱烟花（11264-11457 测试区）**：on_death 排队 shoot → 排队 spawn（烟花=遗嘱 spawn 造物带初速）。run_death_wills 返回值只剩 Vec<PendingSpawn>。
- **操控射击（1655-1700，ctrl_shoots）**：WsShoot 意图改走 spawn 路径——炮塔解算器 solve_launch_vector（2584-2650，保留不动）解出初速后，在操控目标位置 spawn 一发 dynamic 子弹造物（glyph="·" 或同款、radius 0.15 同款、mass 1.0、空 payload）。**操控射击的手感（速度 12 基准+阻力补偿+上限 17.3）必须不变**——解算器不动，只换载体从"世界配发弹丸"到"spawn 造物"。
- **出生重叠策略注意**：spawn 出生点与既有实体身体重叠会被拒绝（#030 既有）。操控射击/造物发射的出生点=发射者自身位置附近，会撞自己身体。处理：**发射点外推**——出生点沿发射方向前移 (r_self + r_bullet + 0.1) 格（发射者自身半径+子弹半径+余量），这是物理诚实的外推不是瞬移（同一拍内从身体边缘出膛）。此逻辑放主程序 spawn 构造处，入档为投掷器模板教学点。
- **ATTACKER_WAT/DEFENDER_WAT/SPY_WAT 不动**（无 shoot 调用）。players/ 目录 .wat 不动（bridge-demo/jumper/pioneer 无 shoot；me.txt 是历史清单，末尾补一行"D-018 后 shoot 已退役，远程=spawn 投掷器"即可）。
- **net.rs entity_kind（196）**：PROJECTILE_CODE 分支保留但加注释（历史实体/兼容判定；新造物弹 kind=creation_dynamic 前端已按圆渲染）。前端 index.html 的 kind==='projectile' 分支（360/664/697/870-903/1113）**暂不动**——残存老弹丸的消亡检测保留一个过渡期，本刀不动前端（防同拍双改互踩；CC 正在跑 D-004b 渲染任务改 static/，git 分文件提交不冲突）。

### ⑥ 测试迁移原则

- 每个 shoot 测试问三个问题：这测的是 import 语义（删）/弹道物理（改通用路径）/战术行为（改 spawn 发射后断言不变）？
- 弹道测试同态改写模板：原 push_shoot(map, shooter, vx, vy, mass) → 构造 dynamic 造物 entity + set velocity（或 spawn 初速落库路径），走 step，断言射程/弧线/CCD 命中不变。
- **物理常数以代码实测为准，不凭任务书记忆**。弹道锚：标准上抛弹 vx=4/vy=+9.8/mass=1 射程≈3.9 格（CCD 重标后口径）。
- 全量 cargo test 计数不得低于现基线（当下约 1063 绿——以你跑出的实际数字为准）。

### ⑦ 新手文档：投掷器模板第一课（docs/newbie-first-thrower.md 新建）

#045 拍板的处置范式落地：shoot 退役转教学。内容（中文，读者=未来的造物玩家）：
- 第一课：拼一个投掷器。完整可抄的 WAT 模板（spawn 一体式：子弹代码内嵌在投掷器代码里，出生初速+小半径+dynamic；分离式一笔带过指向 api.md field_pulse）。
- 讲解组装三步：造物身体（spawn 参数）→ 给它初速（第 11/12 参）→ 接触结算（on_contact 代码攻，指向 api.md）。
- 发射点外推技巧（交付物⑤的 0.1 格余量法）作为教学点写入。
- 历史注记：shoot 曾是世界 API（#034 立法）后被 #045 退役的原因一段（世界投递代码合规/世界不配发身体）。
- api.md 的 shoot 退役说明段指向本文档。

## 完工标准（客观可查）

- G1 `git grep -n "env\" \"shoot\"\|PendingShoot\|step_projectiles\|LaunchTrace\|PROJECTILE_MAX_AGE" -- crates/ bin/codeworld/src/` 无结果（测试区同删）。
- G2 `git grep -n "shoot" docs/api.md` 只剩退役说明段（一处）+历史锚。
- G3 `cargo test --workspace` 全绿，计数 ≥ 基线（以实际跑出为准，报告写明数字）。
- G4 `cargo clippy --workspace` 无新 warning。
- G5 H 搅局者弹道回归：hunter 测试改写后断言 H 的抛射弹道不变（每 8 拍一发、两拍回弧系数、命中静态目标测试锚 hunter_shoots_projectile_at_eighth_tick_and_hits_static_victim 同态保留）。
- G6 操控射击回归：ws_shoot_intent 三测试（8972/9014/9033）同态改写后通过（速度/限流/边走边射语义不变——限流改由"每拍 1 次 spawn"承担）。
- G7 投掷器模板文档存在且内容完整（五段：模板/三步/外推/历史/指针）。
- G8 game-menu.md 三条 🪦 行改 ✅ 划线+api.md 同步。

## Gate（自审查循环）

- 完工后自审：对照 G1-G8 逐条 grep/跑测核实，把每条的验证命令+实际输出写进报告。
- 发现不符：修复后重跑 Gate，循环至全过。
- 报告写 .cc-scratch/cc-report-d018.md（注意不是 $LOCALAPPDATA/Temp——本项目惯例），分节：改动/测试（计数）/冒烟（G5/G6 测试名+结果）/G1-G8 逐条证据。
- **超 100 工具调用**：写 5 行 handoff 到 .cc-progress.md 后重启会话续跑。

## 已知信息（不需要查，直接用）

- **cargo 转发器**：`node $LOCALAPPDATA/Temp/cc-cargo.mjs <args>`（bash 丢 PATH 是常态，直接用转发器）。
- **spawn 17 参签名**（api.md 214）：code_ptr, code_len, x, y, glyph_ptr, color_ptr, radius, mass, binding, motion, vx, vy, blob_ptr, blob_len, field_radius, field_gravity_scale, field_drag_delta → i32。
- **spawn 初速校验**：有限性+模长 ≤ 世界极速 25（严格 > 拒绝/恰等放行/NaN 拒绝），import 层已实现（2026-10-21 落地）。
- **motion 值**：0=static（默认钉死）/1=dynamic（全活受完整物理）。dynamic 造物 kind=creation_dynamic，前端已按圆渲染。
- **field_pulse**（第 15 个 import，2026-10-22 落地）：分离式发射器，圆域内给带速度实体速度加 (vx,vy)，校验五条见 api.md 317-321。
- **接触通道**：on_contact(other_x, other_y) 边沿触发（#030），阶段二 c 步全量扫描+差分，sandbox.rs call_on_contact（1793）。
- **物理常数现值**：重力 -9.8/阻力 0.05/极速 25（config.max_speed），以代码为准。
- **主 tick 管线顺序**（main.rs 1740-1930 一带）：阶段一沙箱唤醒→意图收集→阶段二 spawn/attack/move/物理/接触/攻击投递→遗嘱→清理。
- **旧弹丸三常数**：半径 0.15/寿命 30 拍/每拍 1 发，全在 step_projectiles 与 shoot import 段。
- **测试基线**：约 1063 绿（D-012 收尾时 1063，以你实际跑出为准，只许增不许降）。
- **另一 CC 进程**正在跑 D-004b 渲染任务（改 bin/codeworld/static/），与本刀文件不相交（本刀不碰 static/），git add 只加本刀文件，禁止 git add -A。
- **本机 git**：master 直改，一次 commit 收口，消息：`feat(core,physics,arena,bin,docs): D-018 弹丸退役工程——shoot 移除/弹道通用化/痕迹退役/三常数划线/D-H 迁移造物弹/投掷器模板第一课`。
- **报告路径**：`.cc-scratch/cc-report-d018.md`。commit 后报告补 commit hash。
- **api.md/game-menu.md/dev-plan.md 的 D-018 段**收尾同步：dev-plan D-018 标题加 [✅ 已落地 2026-10-22 · commit <hash>]，progress.md 登记一行。
