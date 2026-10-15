# TASK（期四第六刀·大刀）：半径物理——格子降级为地址系统，实体不可重叠精确化为半径碰撞

## ⚠️ 必须全程 TDD（每个行为先写测试看红，再实现到绿）；这是宪法级改造，边界规则已在任务书写死，禁止自由发挥

## 宪法依据（本刀同时立两条修正案，写进 decisions.md）

**#030 实体不可重叠·半径化**：一格一物（占位铁律）的立法本意是"实体不可重叠"，本刀将执行从"整数格不重叠"精确化为"物理半径不重叠"。活动实体（player/npc）= 圆（radius，默认 0.5，spawn 可自定义）；静态造物（creation）= 方块（AABB，多格占地，格子对齐——Factorio 建筑模型）。格子保留为：地址系统（快照 viewport/渲染投影/造物对齐），不再是物理牢笼。

**#031 攻击点命中制**：attack_at(x,y) 打的是一个点；命中判据从"坐标精确相等"改为"点落在实体身体内（圆：距圆心<r；方块：点在 AABB 内）"；半径内多个实体命中最近者（按 EntityId 升序定序，消灭随机性——延续两阶段结算立法精神）。攻击语义铁律（#001-004：打代码不打血条、掠夺只属击杀）一行不动。

## 业界对标

Factorio（格子世界+移动单位小数碰撞盒 1.4×2.0+建筑多格对齐——千万销量验证）；MDN 圆碰撞（dist < r1+r2）；圆-AABB（钳制点求距离）教科书标准；MMO 胶囊统一判定（Reddit 从业者共识）。

## 项目上下文

项目：D:/projects/CodeWorld（基线 594 测试全绿，git eb0bfe0 之后有 start.bat 修复 6ba0e3c）
关键现状：
- Entity 结构：crates/core/src/entity.rs（pos: (f64,f64)，无 radius）
- 占位：crates/core/src/occupancy.rs（cell_occupied_by=同格查找——本刀退役）
- 移动应用：main.rs apply_pending_changes（SetPos 走 cell_occupied 拒绝——换半径判定+分轴滑动）
- 攻击命中：main.rs settle_attacks / deliver_attacks（e.pos()==atk.target_pos 精确相等——换点命中）
- 接触：ContactPair（同格触发——换圆相触+边沿触发）
- 重力/地面：crates/physics/src/default.rs（gravity -9.8/拍；地面钳 y=0——本刀改钳 y=radius）
- spawn 签名：(code_ptr, code_len, x, y, glyph, color)->i32（本刀加第 7 参 radius f64——向后兼容：WAT 侧不传用默认 0.5 的方案不可行，直接改签名+同步改现有 .wat 调用点）
- 快照：net.rs entities[{id,glyph,color,x,y,kind,name,controlled,owner}]（本刀加 radius）
- 前端：static/index.html（插值/预测镜像 applyMoveSteps——碰撞模型变了，被挡语义从"整步作废"变"分轴滑动"，预测镜像同步）
- A/D/S/H 四生物+P 都是单格小实体（等效 r=0.5），塔 T 是 creation

## 需求（边界规则全部写死）

### 1. Entity 加 radius（core）
- `radius: f64`（活动实体圆半径；creation 不用它，用占地 AABB）
- spawn_env 第 7 参 radius；校验 0.05 ≤ radius ≤ 25.0（超界 warn 并钳制）
- 现有 .wat（pioneer/example）调用点同步补参（0.5）
- 四生物+A/D/S/H/P 默认 0.5；解析器/测试构造器同步

### 2. 碰撞判定（core 新模块 collision.rs 或并入 occupancy 重写）
- `circle_overlap(a_pos, a_r, b_pos, b_r) -> bool`：dist² < (a_r+b_r)²（用平方距离免开方）
- `circle_aabb_overlap(c_pos, r, aabb) -> bool`：钳制点到 AABB 求距离 < r
- creation 的 AABB：由其占地格推导（单格塔=1×1 对齐格；多格造物留接口，MVP 塔就 1×1）
- 单元测试：相触边界（恰好=r 不算重叠，用 < 严格）、包含、分离、圆-AABB 角/边/内

### 3. 移动应用（main.rs apply_pending_changes 重写拒绝逻辑）
- 新 SetPos 目标位若与任何实体重叠 → **分轴滑动**：先试整步；撞则试 (new_x, old_y)，再试 (old_x, new_y)，都撞则原地不动——贴墙滑而非整步作废
- 落地钳制：y < radius → y = radius（脚贴地，圆心即位置）
- 被挡日志改 INFO（有滑动信息：全挡/滑 x/滑 y）
- **不推挤**（撞=停或滑，不位移对方——用户明确否决过推着走）

### 4. 接触通道（边沿触发，新规则写死）
- 判据：圆相触（creation 用 AABB 版）
- **边沿触发**：每实体对维护"上拍已接触"集合，本拍新进入接触=触发 ContactPair；持续贴着不重复；分开后再碰=再触发
- 实现：接触集合放 tick 间状态（HashMap<EntityId, HashSet<EntityId>>，双向对称记录，每拍重建）

### 5. 攻击命中（main.rs settle/deliver_attacks）
- attack_at(x,y)：找"点在体内"的所有存活实体（圆：dist<r；方块：点内），命中其中**最近者**（距离同按 EntityId 升序）
- 无一命中=落空（照旧 miss）
- 两阶段结算/掠夺/拦截语义零改动

### 6. 重力与站立（physics/default.rs）
- 重力照拉 vy（-9.8/拍）
- 地面钳制：y < radius → y=radius（不再钳 0）
- 摔在别的实体上=接触即停（堆叠合法：站在谁身上都行，底下的走了上面的自然下落——物理诚实，接触边沿触发防刷屏）

### 7. 操控层（input.rs next_move_step + main.rs 意图路由）
- 每拍步长 1.0 朝目标方向走（**矢量方向**：dx,dy 归一化×1.0——斜走一步到位，L 形规则废除）
- 到达判定：距目标 < 0.5 停（含半径因素：目标点是点击点本身）
- 悬停力独立：空中（y > radius）每拍升力 +9.8 抵消重力——**移动被挡/滑动时升力照出**（被挡≠坠落，上一刀的 bug 根治）；地面实体不喷
- 飞行相位状态机简化：爬升/平飞合一（矢量方向自然覆盖），保留悬停态

### 8. 快照与前端（net.rs + index.html）
- 快照 entities 加 radius（协议 v1.5，net-protocol.md 更新）
- 前端：实体按 radius 画圆轮廓（字符照画，圆圈大小=身体）；预测镜像 applyMoveSteps 同步分轴滑动+矢量步（#028 镜像铁律）
- 渲染网格线照画（地址系统视觉保留）

### 9. 文档
- decisions.md：#030/#031 全文（按项目宪法格式：定案/原因/否决/落点）
- api.md：spawn 第 7 参 radius 说明（示例更新）
- net-protocol.md v1.5：radius 字段+移动语义（矢量步/分轴滑动/悬停独立）
- progress.md 更新

## 验收标准

1. TDD：上述每个模块的红→绿证据（会话里能看到先红后绿）
2. cargo test --workspace 全绿；老测试中因模型变化失效的：**修改断言以反映新语义**（报告逐条列出改了哪些、为什么），禁止删除测试
3. clippy/check 三连过
4. 土豆浏览器验收：
   a. 点左上方 → P **斜线**飞过去（非 L 形）
   b. P 撞塔 → **贴着滑过去**（非整步卡死）
   c. 悬停中撞 S → 不再坠落（升力独立）
   d. 点 D 攻击 → D 在半径内=命中（站桩目标必中，猫鼠博弈仍在：移动目标打不中）
   e. battle.log 看到接触边沿触发（进接触一行，持续贴着不刷）
5. 审查聚焦：分轴滑动的边界（双轴同时撞/滑入第三实体）；接触集合的每拍重建正确性；点命中多实体定序；预测镜像与服务器逐案一致；radius 校验边界

## 已知信息（不需要查）

- 重力 -9.8/拍是现值（crates/physics/src/default.rs），悬停补偿 +9.8
- 世界上限 y=25（viewport）；x 范围 -50~50
- 四生物初始位：A(5,0)/D(15,0)/H(10,8 出生即落)/S(25,12)/P(30,0)；塔 T 由 pioneer 第 3 拍 spawn 于 P 东侧
- 现有 594 测试分布：core 100+/arena 30+/codeworld 400+（移动/攻击/占位相关约 60-80 个需断言更新——工作量主要在这里）
- WAT 侧 spawn 调用点：players/pioneer.wat（locals 显式写法，加参数照抄模式）
- 快照 JSON 序列化在 net.rs（serde），加字段是纯加法
- 前端 applyMoveSteps 在 index.html PURE 区（机器验证区），同步后跑 node 验证脚本确认

## 硬 Gate

```bash
cd D:/projects/CodeWorld && cargo test --workspace
cd D:/projects/CodeWorld && cargo clippy --workspace --tests -- -D warnings
cd D:/projects/CodeWorld && cargo check --workspace
```

## 审查（单审一轮，外部模型）

聚焦见验收 5。另查：测试是否被削弱（改断言≠删检查）；#030/#031 文本是否准确入宪；api.md/net-protocol.md 与实现一致。

## 交付物

1. 中文 commit（feat: 期四第六刀——半径物理#030#031：碰撞/接触/攻击/移动四判据升级）
2. C:/Users/Administrator/AppData/Local/Temp/cc-report.md（四节：改了什么/怎么验证/断言更新清单+理由/土豆验收步骤）

不要弹选项；信息缺失超3次试错即停写报告。
