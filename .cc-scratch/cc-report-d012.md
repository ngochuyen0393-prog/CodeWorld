# D-012 力场系统 收工报告

- **commit**: dc74819（master，19 文件，+2199/-111）
- **Gate**: `cargo test --workspace` **1000 passed / 0 failed**（基线 952 + 新增 48）；`cargo clippy --workspace` **0 warning / 0 error**（提交树上二次复验）

## 一、8 项交付物逐条对账

| # | 交付物 | 做没做 | 落点 | 测试锚 |
|---|--------|--------|------|--------|
| 1 | field 出生声明（spawn 第 15/16/17 参） | ✅ | core/entity.rs（三字段+serde default+clamp 三函数+getter/setter 直存）；arena/sandbox.rs（PendingSpawn 三字段+env.spawn 17 参校验+INFO 带三值）；main.rs process_spawn_requests（三 setter 直存）；破坏性变更全调用点同刀修完（pioneer.wat/.txt、main.rs birth_motion_spawner_wat 等 4 生成器+折叠式 once_spawner、arena 内嵌 WAT×7） | core clamp×9+defaults/setters/roundtrip/backcompat×4；arena 透传+钳制×4；main 17 参端到端 spawn_17_params_field_declaration_lands_in_entity |
| 2 | 零计费 | ✅（什么都不做条款） | 全仓无任何场专用计费代码（grep 核对：场路径零 fuel 扣除/零费用结构体）；api.md「力场」节载明"场的成本=普通造物日常开销+实体名额" | —（阴性条款，无测试对象） |
| 3 | 叠加 v2（代数抵消+域钳制） | ✅ | physics/fields.rs `effective_gravity_drag` 纯函数：重力叠乘、阻力 Σdelta、域钳重力 [-98,0]/阻力 [0,25]；无场快速路径逐位=默认值（零回归锚）；优先级未实现（定案）；账本无所有权标签 | effective_no_fields_matches_defaults_exactly / single_field_multiplies_gravity / single_field_adds_drag_delta / two_fields_algebraic_cancellation / domain_clamp_bottoms_out |
| 4 | 协议 v3（on_field 询问应答） | ✅ | arena/sandbox.rs `call_on_field`（第 14 个玩家函数契约）：场信箱=沙箱线性内存末尾 516B（n 4B+64 槽×8B，世界保留）；域内清单 EntityId 低 64 位 LE 升序、不含场源；返回 0=维持声明/非 0=本拍 drag_delta 覆盖；独立 fuel 预算包裹（信箱写信成本不扣场源账，on_contact 同款三步）；未导出/Err=按出生声明结算 | call_on_field_skips_if_no_export / writes_ids_and_reads_answer（含 fuel 恢复断言）/ signature_mismatch_is_err_not_panic |
| 5 | 物理结算接入 | ✅ | physics/default.rs：step/step_projectiles 加 `fields: &[FieldEffect]` 参数（main 每拍收集传喂）；域判定=圆心距²<r²（严格域内）；FieldEffect{source_id,pos,radius,gravity_scale,drag_delta}；aloft 优先级高于场（短路在前）；弹丸=第一消费者；static 造物下落/零速落点/冲量路径三处重力全部吃生效值 | step_entity_in_zero_gravity_field_does_not_fall / step_entity_outside_field_unaffected_vs_baseline（逐位对照）/ step_projectiles_field_changes_trajectory |
| 6 | 可视铁律+成型期 | ✅ | core/physics.rs `FIELD_FORMING_TICKS=3`；config.rs CODEWORLD_FIELD_FORMING；collect_fields 按 created_tick 过滤；net.rs FieldSnapshot{x,y,radius,formed}+WorldSnapshot.fields（空数组恒有）；index.html 半透明圆环（成型中虚线 0.22 alpha/成型实线 0.38 alpha，arc stroke 一圈无粒子） | collect_fields_excludes_forming_fields；main snapshot_contains_fields_with_forming_flag / field_forming_ticks_boundary_wiring |
| 7 | tick 刷场登记制+边界防抖 | ✅ | collect_fields 每拍全量重算（无 enter/exit 缓存）；边界冷却防抖**不做**（on_field 是询问非事件——decisions #052 注记：场代码自记边沿，世界级冷却待事件语义再议） | collect_fields_sorted_by_id_and_applies_overrides（定序）/ skips_dead_and_no_field_entities |
| 8 | 分离式发射器 field_pulse | ✅ | 第 15 个 env import：PendingFieldPulse 排队；校验五条（有限性/冲量模长≤25 严格>拒绝恰等放行/radius 钳 [0.1,25]/中心±10 格/1 次每拍限流）；阶段二 apply_field_pulses（EntityId 升序、推完极速钳制、velocity≡0 推不动、被推者 id/推前推后速度 INFO）——D-018 初速来源就绪 | arena field_pulse×7（排队/非有限/超模长/恰等/radius 钳/±10/限流）；main field_pulse_pushes_dynamic_creation_in_domain / field_pulse_pushes_projectile_in_domain |

## 二、新测试清单（48 个，TDD 先红后绿）

- **core（+14，206→220）**：clamp_field_radius 三态×3 / clamp_field_gravity_scale×3 / clamp_field_drag_delta×3 / defaults / setters 直存 / serde roundtrip / 旧快照 backcompat
- **arena（+14，111→125）**：spawn field 透传+三参钳制×4；on_field 未导出/通信+预算恢复/签名 Err×3；field_pulse 排队/非有限/超模长/恰等放行/radius 钳/±10/限流×7
- **physics（+12，73→85）**：effective 五锚×5+域外×1+collect×3（成型/死者无场过滤/定序+应答覆盖）；step 接入×3（域内不落/域外逐位基线/弹丸改弹道）
- **bin（+8，402→410）**：17 参端到端/场影响生物落点/pulse 推造物/pulse 推弹丸/快照 fields/成型期接线×6+config env 覆盖+常数默认×2
- **守卫**：field_demo_example_compiles_and_declares_field（示例即文档，全管线走通+on_field 应答 0）

计数方式：awk 累加全部 `test result:` 行 passed 列（分包：410+125+220+85+72+30+26+16+6+5+5=1000）。

## 三、冒烟说明

- **环境权限边界**：CC 沙箱 cargo 仅 test/clippy/check 可跑，主程序 exe 交互冒烟做不了（历史惯例留人工）。等价保障：pioneer.wat 冒烟测试（真实读盘 players/pioneer.wat 全链路 tick）与 field-demo 守卫测试均在 1000 绿内——破坏性 17 参变更若漏改任一玩家文件，这两条测试当场红。
- **留人工**：浏览器打开 start.bat 肉眼看力场环（虚线 3 拍→实线）+ CODEWORLD_FIELD_FORMING=1 观察成型窗口。

## 四、文档同步（同一 commit）

- api.md：spawn 17 参签名+参数表 3 行；on_field 导出契约节（信箱/返回值/燃料/失败诚实）；field_pulse 节；「力场（D-012）」节（叠加 v2/成型期/零计费/可视铁律）；import 计数 **14→15**（任务书写 13→14，实际基线已是 14 个——birth_blob/thrust 等后续刀已入账，以实际为准）
- net-protocol.md：**v2.3**（快照顶层 fields 数组字段表+版本史）
- decisions.md：**#052**（任务书写 #051——#051 已被 D-017 两刀占用[重复编号]，按"只增不改"纪律顺延 #052，条目内注记）
- dev-plan.md：D-012 条改 ✅（落地实测清单）
- game-menu.md 1.7 常数总账：+3 行（FIELD_FORMING_TICKS=3 / 场参数域 / 脉冲域）
- docs/progress.md：「现在」段更新

## 五、改动文件清单（19 个，逐个列出）

**代码**：crates/core/src/entity.rs / crates/core/src/physics.rs / crates/arena/src/sandbox.rs / crates/physics/src/lib.rs / crates/physics/src/default.rs / crates/physics/src/fields.rs（新）/ bin/codeworld/src/main.rs / bin/codeworld/src/config.rs / bin/codeworld/src/net.rs / bin/codeworld/static/index.html
**玩家文件**：players/pioneer.wat / players/pioneer.wat.txt（17 参破坏性变更）
**示例**：examples/field-demo.wat（新）
**文档**：docs/api.md / docs/decisions.md / docs/dev-plan.md / docs/game-menu.md / docs/net-protocol.md / docs/progress.md

未触碰：.cc-bugs.md/.cc-context.md/.cc-progress.md（兄弟会话在途改动，原样留在工作区）；examples/sentinel.wat（grep 核实无 spawn 调用，无需改）。

## 六、遗留 / 注记

1. **极速场未做**（定案内：本刀只落重力缩放+阻力；家族全景留后续刀）
2. **field_pulse 无 fuel 代价**（任务书校验清单无燃料条目，与 spawn 同款零燃料；若要配上力出力定价另刀）
3. **on_field 窗口 probe 不可用**（probe 桥只挂 on_tick 窗口；api.md 已载明——场代码要 probe 走下一拍 on_tick）
4. **#051 编号已被占用**（decisions 历史重复编号，本刀 #052 顺延）
5. **堆叠场域判定的采样语义**：域判定用实体当拍结算前位置（与全世界快照语义一致），实体在拍内穿越场边界下一拍才感知——诚实口径，无需修
6. **D-018 依赖就绪**：分离式初速=field_pulse 推存量（本刀）+出生初速第 11/12 参（已在库），退役工程可开工
