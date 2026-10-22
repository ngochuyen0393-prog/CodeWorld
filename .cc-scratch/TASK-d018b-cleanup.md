# TASK: D-018b 收尾清扫——grace_owner 立法重接 + 解算器地形拦截 + CCD 端点相切 + 死路径清理

[Superpowers 激活：verification-before-completion 文档级。]

你是 CodeWorld 项目（D:/projects/CodeWorld）的施工工程师。D-018 弹丸退役（9cee29b）+R1（f473d86）后遗留 4+1 项 P2 清扫。**其中第 1 项经战略层核实升级为"立法重接"**：binding=2（grace_owner）是 decisions.md #035 修订四在案立法（"`2`=grace_owner（落地不砸发射者）"），#045 退役清单未废它，但 D-018 把命中改走接触通道后接线断了——PendingAttack.grace_owner 生产恒 false，deliver 的 `target_id == atk.attacker` 永不成立（新链路 attacker=弹实体自身，目标是发射者）。本刀把立法接回新链路。

## 1. grace_owner 立法重接（核心项）

**语义（定死）**：deliver_attacks 现场查 attacker 实体——`atk.attacker` 的 `binding() == BINDING_GRACE_OWNER && atk.attacker.bound_to() == Some(target_id)` → 哑弹（INFO 日志沿用现有"[binding] grace_owner 豁免生效"文案，措辞改准确：造物出刀不结算 bound_to 主人）。契约=出生声明由世界每拍托管，本来就不该每刀携带数据——这是 #035 立法形态在新链路的等价迁移，不是新立法。

- 删 `PendingAttack.grace_owner` 字段 + `projectile_strike()` 构造器 + `contact_exempt` 字段（新链路弹出刀走 on_contact→attack_at=PendingAttack::strike，弹贴身出刀接触门天然成立，两字段生产意义均归零）。attack.rs:14-58 注释同步重写为接触通道链路口径（#045 替代链），**去掉"#034 弹丸结算"退役语义**。
- deliver 的 grace 分支（main.rs:3005）改实体驱动；`!in_reach && !atk.contact_exempt` 简化为 `!in_reach`。
- **CtrlShell 模板 binding 0→2**（main.rs:1605 ctrl_shell_spawns 处）：恢复操控射击"出手不伤自己"的既有 UX（shoot 时代"不撞发射者"飞行全程豁免的同款承诺），走的是立法通道非特殊 hack。注释锚 #035+D-018 迁移。
- api.md:199 binding 行补 `2`=grace_owner（你的造物出刀永不伤 bound_to 主人；不声明则误伤合法——**世界不豁免，豁免是声明**，#035 修订四原文）。
- 测试改造：main.rs:9246 `grace_owner_landing_does_not_settle_shooter` / :9268 `unbound_landing_still_settles_shooter` 改实体驱动（造弹实体 set_binding+set_bound_to，PendingAttack::strike 出刀，不再字面量构造已删字段）。

## 2. 空转测试重造（grace_owner_upward_shell_lands_at_feet_without_self_kill, main.rs:9282）

现状弹出生 (5.75,0.5) 距发射者 (5.0,0.5) 0.75 > 接触窗口 0.65（r弹0.15+r主0.5），on_contact 根本不触发——binding 声明全程零作用，断言空转。重造为**真接触路径**：弹出生 (5.75,0.5)，水平初速 (-3,0) 朝发射者，一拍内 CCD 撞体截断（swept_first_body_contact 首触 t+1cm 推进落进窗口）→ detect_touching 触发 → on_contact 出刀 → deliver 查弹 binding=2+bound_to=发射者 → 哑弹。断言：发射者存活+点数不变+弹停驻（速度清零）。保留同型对照测试：binding=0 的弹同样路径落回来出刀 → 发射者被默认击杀（世界不豁免的回归锚）。

## 3. 解算器地形拦截（P2-1，选"补拦截"非"改口径"）

solve_launch_vector 的 simulate_path（main.rs:2697-2716）落地判据只有平地 `y < r`，非平地世界（D-016 Hills/Mountain）山体挡弹时"解算说能打中=世界真命中"的 #028 iff 契约断裂。修法：

- simulate_path 加 `&Terrain` 参数，每拍位移线段（prev→cur）先查 `terrain.segment_first_contact(prev, cur, r)`（与 step CCD 同判据），Some(t) 则截断到触地点、终止后续帧；原 `y < r` 判据删除（Flat 地形下 segment_first_contact 与其逐位一致，行为零回归）。
- solve_launch_vector 签名加 `terrain: &Terrain`（调用点 main.rs:1578 传 `&world.terrain`）；测试调用点（main.rs:8858 起）补 `&Terrain::flat()`。
- **前端镜像同步（#028 铁律）**：index.html simTrajectory（~L805）同补——terrainData（快照 {x0,dx,heights}，null=平地回落 y=0）分段线性求交的 JS 复刻（每拍线段 vs 地形格线性段，与 Rust segment_first_contact 同口径：逐格沿路径方向解 A+B·t=0 取最先触）。**index.html 是 D-004b 刚大改的文件，只动 simTrajectory/相关常量带，禁碰 3D/genshin-toon 层**。
- 注释锚：main.rs:2671 附近"当且仅当"承诺现在真成立（平地+地形两态），注明拦截与 step CCD 同判据。
- 新测试 2 个：Mountain 固定 seed 下挡山目标（取一个必被山体拦截的位置）解算 None；Flat 下既有射程回归不变。

## 4. CCD t=1 端点相切缺口（P2-7，physics/default.rs:533-535）

现状 `(bt + 1e-2/disp).min(1.0)`：首触点恰为线段末端（bt≈1）时 min 吞掉推进量，停点悬在 detect_touching 的 ≤ 边界靠浮点运气（注释自己承认"边界值浮点向上漂 1 ULP 即漏"）。修法（一行语义连续）：上限从 1.0 放到 `1.0 + 1e-2/disp`——bt 参数化在 1+ε 处线性外推=末端+沿位移方向 1cm，与既有推进语义同族（首触在中途的场景 min 仍取 bt+ε 不受影响）。注释写清外推几何。新增测试：构造首触 t∈[1-1e-9, 1] 邻域的固定坐标（末端距目标圆心 0.65±ULP 级，坐标预先算死写进测试，禁随机），断言撞体截断后停点到目标圆心距离 ≤ r和（落进接触窗口）。

## 5. 顺手项

- enforce_binding_deaths 未用参数 `_sandbox_map` 删除（定义 main.rs:2868+调用 :1805+测试 :9234 三处）。
- docs/ideas.md 日志段 append 一行（火花池零摩擦捕捉，别的不动）：`[2026-10-22] ⏸待时机｜寿命管理第二课文档（D-018b 附带想法，土豆未拍板）：命中后未自杀的造物弹停驻地面持续占实体名额（上限 64）——投掷器教学第二课候选（第一课=拼装，第二课=寿命管理：hit 后自毁/燃料自尽）。是否立项做文档由土豆定。`

## Gate（全过才算完）

- G1 全量 `cargo test --workspace` ≥1074 绿 0 failed（亲跑，awk 累加 test result 行报数）。
- G2 clippy 本刀 hunks 无新 warning。
- G3 grep 证据：PendingAttack 无 grace_owner/contact_exempt 字段；deliver 有 binding()+bound_to() 驱动的豁免分支；CtrlShell binding: 2；simulate_path 有 segment_first_contact；physics 有 `1.0 + 1e-2`；api.md 有 grace_owner；ideas.md 有寿命管理行。
- G4 前端：simTrajectory 含 terrainData 分支（grep terrainData 在 simTrajectory 函数体内），3D 层零触碰（git diff 只含弹道带 hunks）。
- G5 一次 commit，消息 `feat(core,physics,arena,bin,web,docs): D-018b 清扫——grace_owner 立法重接接触通道+解算器地形拦截+CCD 端点相切+死路径清理`。只 add 本刀文件。
- 报告：.cc-scratch/cc-report-d018b.md（改动/测试输出/G1-G5 证据）。

## 已知信息（不需要查，直接用）

- cargo 转发器：`node $LOCALAPPDATA/Temp/cc-cargo.mjs <args>`（PATH 丢 cargo 是常态）。
- 接触窗口 0.65 = CTRL_SOLVE_TOLERANCE = r弹 0.15 + r目 0.5；detect_touching 判据含等号（≤）。melee_in_reach 严格 <（collision.rs:56）。
- 弹 bound_to 写入点：process_spawn_requests main.rs:2504 `set_bound_to(Some(req.creator))`。
- segment_first_contact_circle 恰相切（disc≤0）返回 None——端点相切测试要构造"末端微量入圆/出圆"而非数学恰切。
- 常量以代码现实现为准，别信本任务书记忆。
- master 基线 f473d86，测试 1074 绿。Terrain 构造 `Terrain::new(TerrainPreset::Mountain, seed)` / `Terrain::flat()`（crates/core/src/terrain.rs:146/208）。
- 报告出现 ≠ 收尾：commit 后才算完。
