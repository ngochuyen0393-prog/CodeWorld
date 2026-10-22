# D-018b 收尾清扫 施工报告

日期：2026-10-23 ｜ 基线：f473d86（施工期间 D-005 决斗刀0 并行落地 0f882d3，本刀 hunks 与其零重叠）
模式：TDD 全程红绿（每项先看测试失败再实现，RED 实证记录见下）

## 一、改动文件清单（8 个，全列）

| 文件 | 改动 |
|------|------|
| `crates/arena/src/attack.rs` | 删 `PendingAttack.grace_owner`/`contact_exempt` 字段+`projectile_strike()` 构造器；模块头与 `strike()` 注释重写为 #045 替代链接触通道路径（豁免归实体出生声明，不在刀上携带）；字段存取测试同步收敛为三件套 |
| `bin/codeworld/src/main.rs` | ① deliver_attacks 豁免分支改实体驱动（`binding()==BINDING_GRACE_OWNER && bound_to()==Some(target_id)` → 哑弹，INFO 文案改"造物出刀不结算 bound_to 主人"）；接触门 `!in_reach && !atk.contact_exempt` → `!in_reach`。② 操控射击模板 binding 0→2（注释锚 #035 修订四+D-018 迁移）。③ solve_launch_vector 加 `terrain` 参，simulate_path 每拍位移线段查 `terrain.segment_first_contact`（旧 `y<r` 判据删除），文档注释补 iff 两态成立锚；全部 10 处调用点补参（生产 1 处 `&world.terrain`、测试 9 处 `&Terrain::flat()`）。④ sim_trajectory 测试镜像同步地形截断。⑤ grace 测试三件重造+unbound e2e 对照新增（见三）。⑥ 删 `projectile_strike_bypasses_contact_gate`（构造器已退役，行为由 unbound e2e 对照覆盖）。⑦ `enforce_binding_deaths` 未用参数 `_sandbox_map` 删除（定义/调用/测试三处）。⑧ ws_shoot 绑定断言 0→2 |
| `crates/physics/src/default.rs` | CCD 撞体截断推进上限 `(bt+1e-2/disp).min(1.0)` → `.min(1.0+1e-2/disp)`（bt 参数化 1+ε 线性外推=末端沿位移方向 1cm），注释写清外推几何与锚测试；新增端点相切锚测试 |
| `crates/core/src/entity.rs` | BINDING_GRACE_OWNER 常量注释同步接触通道口径（原"弹丸落地结算不伤发射者"已无对应机制）。**任务书未列此文件——语义重接的直接连带（注释陈述已被删除的机制），最小 3 行注释改动，特此报备** |
| `bin/codeworld/static/index.html` | simTrajectory 带 3 处 hunk（784-870）：新增 `terrainFirstContact`（Rust segment_first_contact 的 JS 逐位复刻：域外钳边格/方向序遍历/段内根校验/边格延长线放行）+ simTrajectory 地形拦截分支（`terrainData==null` 回落旧 `y<PROJ_BODY` 判据零变化）。**3D/genshin-toon 层零触碰**（diff hunk 全在弹道带） |
| `docs/api.md` | binding 参数行补 `2`=grace_owner（"你的造物出刀永不伤 bound_to 主人；不声明则误伤合法——世界不豁免，豁免是声明"） |
| `docs/ideas.md` | 追加 1 行：寿命管理第二课文档候选（⏸待土豆拍板） |
| `docs/progress.md` | 「现在」节更新为 D-018b 落地，D-005 移「上一轮」 |

## 二、各项 RED→GREEN 实证

### 1. grace_owner 立法重接
- **RED**：测试先改实体驱动（造弹实体 set_binding(2)+set_bound_to(发射者)，`PendingAttack::strike` 出刀）——`grace_owner_landing_does_not_settle_shooter` 与新 e2e `grace_owner_shell_return_strike_duds_without_self_kill` 双双失败（哑弹未生效/发射者被杀移除，panicked 输出在案）；unbound 两测先行绿=回归锚。
- **GREEN**：deliver 实体驱动分支落地后 grace 2 绿+unbound 3 绿+ws_shoot 9 绿。
- **空转测试重造**：旧 `grace_owner_upward_shell_lands_at_feet_without_self_kill` 弹出生 (5.75,0.5) 垂直上抛，落点距发射者 0.75 > 接触窗口 0.65，on_contact 从未触发、binding 全程零作用——断言空转（任务书判明）。重造为 `grace_owner_shell_return_strike_duds_without_self_kill`：水平初速 (-3,0) 回打，一拍内 CCD 撞体截断（swept 首触 t≈0.047+1cm 推进落进窗口，停点距发射者 0.648≤0.65）→ detect_touching → on_contact 出刀 → deliver 查声明 → 哑弹；断言发射者存活+点数不动+弹停驻速度清零。同型对照 `unbound_shell_return_strike_kills_shooter_world_no_exemption`：binding=0 同路径 → 发射者被默认击杀移除（世界不豁免回归锚）。

### 2. 解算器地形拦截
- **签名先行+RED**：`solve_launch_vector` 加 `terrain: &Terrain`（全部调用点同步），Mountain 测试先行——临时禁用拦截（`Option::<f64>::None` 替换判据）实证 RED：解算返回 Some（弹道穿山而过），`seed=9 peak=(38.0, 8.0)`；恢复实现 GREEN。
- 测试设计：固定种子表确定性探测（首个满足"发射位低地+x∈[32,52] 有 ≥3 格正峰"的 Mountain 种子，零随机源零机器数据）；目标埋峰体下 1 格（任何到达路径必先穿山面，触地点距目标 ≥1.0 > 窗口 0.65 → None）；平地对照同坐标 Some+弹道进窗口（证明 None 归因地形拦截而非火力上限）。
- **平地零回归**：既有 6 个解算器测试全部原断言通过（截断判据在平地与旧 y<半径 同语义，末点精确到贴面）。
- **前端镜像**（#028 铁律）：`terrainFirstContact` JS 复刻+simTrajectory 分支；node 冒烟实证：平地（null）落点与旧行为一致零变化；6 格高墙立 x∈[34,36.5] 弧线停在墙前 (34.20, 5.02)=拦截生效。

### 3. CCD 端点相切（P2-7）
- **ULP 实测定标**：临时探针扫端点偏移谱 [-8,+8) ULP——实证两类缺陷形态：k=0（数学恰切）二次方程 t>1 整个 None（迟一拍自愈）；k=-1/-4（恰切点下方 1-4 ULP）截断发生但停点距离 0.65000000000000021/…075 > 0.65=detect_touching 漏接触——后者即 min 上限吞推进量的真实作用域。
- **RED**：定稿坐标（端点=恰切点 9.35 下方 1 ULP=9.349999999999998，重力 0 引擎，固定字面量禁随机）→ 测试失败 `实际 0.6500000000000021`。
- **GREEN**：上限放宽 `1.0+1e-2/disp` → 通过，physics 全测 104 绿（含锚测试，探针已删）。

### 4. 顺手项
- `enforce_binding_deaths` 签名删 `_sandbox_map`（定义/调用/测试三处），绑定契约测试全绿。
- `entity.rs` 常量注释 3 行同步（关联文件报备见上表）。
- ideas.md 寿命管理行已追加。

## 三、Gate 证据

- **G1**：`cargo test --workspace` 亲跑——`test result` 18 段全 ok，awk 累加 **1092 passed / 0 failed**（≥1074 ✓；构成=基线 1074+决斗 16=1090+本刀净增 2：Mountain 拦截/CCD 端点/unbound e2e 三个新测试 −1 删旧弹丸接触门测试）。日志：`.cc-scratch/d018b-g1-test.log`
- **G2**：`clippy --workspace --all-targets` 亲跑——全树 6 条 warning 全在他方 hunk（main.rs:5028/8244、net.rs:1232、avatars.rs:501、mask.rs:448、liquid.rs:381，均为 D-005 区/HEAD 既有），**本刀 hunks 零新警告** ✓。日志：`.cc-scratch/d018b-g2-clippy.log`
- **G3**：grep 证据七连——① attack.rs 结构体体内仅 attacker/target_pos/payload 三字段（grace_owner/contact_exempt 仅存于模块注释的退役记事）；② deliver 有 `e.binding() == codeworld_core::entity::BINDING_GRACE_OWNER && e.bound_to() == Some(target_id)` 驱动豁免分支（main.rs:3116）；③ CtrlShell `binding: BINDING_GRACE_OWNER`（main.rs:1668）；④ simulate_path 有 `terrain.segment_first_contact`（main.rs:2806）；⑤ physics 有 `(bt + 1e-2 / disp).min(1.0 + 1e-2 / disp)`（default.rs:542）；⑥ api.md:203 有 grace_owner；⑦ ideas.md:42 有寿命管理行 ✓
- **G4**：index.html diff 恰 3 hunk（@@ -784/-792/-805 全在弹道带），simTrajectory 函数体内含 `terrainData == null` 分支+terrainFirstContact 调用；3D/genshin-toon 层零触碰 ✓
- **G5**：单 commit（见 git log），只 add 本刀 8 文件，不含 .cc-bugs/.cc-context/.cc-progress/.cc-scratch。

## 四、发现告知（未动，等拍板）

1. `docs/newbie-first-thrower.md` 教学模板注释仍写 `binding 0=unbound`（任务书改动面未含教学文档；教学模板主体=玩家手写 WAT，是否让"第一课"同享 grace 语义由土豆定，改动=一行）。
2. 操控射击 e2e（ws_shoot 系）中解算初始速度的弹若落回发射者贴旁，现在会被 grace 声明豁免——"出手不伤自己" UX 恢复且升级为声明语义（原 shoot 时代飞行全程豁免的等价物），无额外回归面。
