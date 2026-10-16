# TASK（期五第三刀·大刀）：弹道系统——离体物理立法 #034 + shoot 弹丸 + attack_at 接触化

## ⚠️ 必须全程 TDD（每个行为先写测试看红，再实现到绿）；这是宪法级大刀，边界规则已全部定死（docs/design-ballistics.md 设计稿 + 三天讨论），禁止自由发挥超出图纸的玩法设计

## 宪法依据（本刀立 #034，写进 decisions.md）

**#034 离体物理与弹道涌现**（立法内容，从设计稿直取）：
1. 离体即世界物体：任何东西离开一个身体，即成为世界实体（逻辑+物理+渲染三层同体），受世界全部物理定律约束。出手前归代码，飞行中归世界，命中后归结算。
2. 重力常量、阻力常量：世界基石参数，对一切离体之物生效，不豁免不分类。
3. 阻力减速 = 阻力 ÷ 质量：除法是定律，质量为分母。射程不立属性，全部物理涌现。
4. mass 是物理人格属性：与 radius 并列。工程校验 0.05 ≤ mass ≤ 100（除零护栏，非玩法裁定）。**铁律：mass 只参与物理反应（弹道/动量），永不参与攻击结算——攻杀仍是代码对代码**。
5. 弹道、射程、攻击效果全部涌现：世界不定义任何攻击形态（无弹种表/无激光分类/无直飞特权）。玩家想要什么效果自己写参数拼（#033 分工总纲：我们只给物理规则，玩家决定如何对接产生什么效果）。

## 设计稿（必读，所有细节以它为准）

docs/design-ballistics.md——血统认证/立法五条/实现切分/调研引证/否决方案清单全在。任务书与设计稿冲突时**以设计稿为准**并报告。

## 项目上下文

项目：D:/projects/CodeWorld（基线 675 测试全绿，git 3d0f93f）
现状关键点：
- Entity 已有 radius（#030 半径物理）；mass 是本刀新增字段（serde default 1.0，旧快照兼容，抄 radius 先例）
- 重力 -9.8/拍在 physics step（crates/physics/src/default.rs）；aloft 免重力集合已有（hover/操控悬停在用）——弹丸不吃 aloft（它就是要吃重力的）
- spawn 签名现 7 参（code_ptr, code_len, x, y, glyph, color, radius）——本刀加 mass 第 8 参（向后兼容同步改 .wat 调用点，抄 radius 先例）
- attack_at 现状：瞬发点命中（#031 点在身体内即中，无距离限制）——本刀改接触判定
- 快照 v1.7：entities 有 id/glyph/color/x/y/kind/name/controlled/owner/radius/disposition/wait_left
- 前端：Canvas+插值+预测+相机；弹丸渲染新加；刀光装饰动画（attackFx/attackLines）在真弹道落地后**保留给 attack_at 贴身刀**（贴身刀瞬发，无飞行体，装饰动画仍合适）
- 物理步顺序：阶段二 apply_pending_changes → 物理步（重力/落地钳制/堆叠）→ 接触扫描（边沿触发）

## 需求（边界全部写死）

### A. 基石层：阻力常量 + mass（Rust）
1. config.rs 加 `drag_coefficient`（env CODEWORLD_DRAG，默认值 CC 实测定——判据：默认参数下弹丸射程 5~15 格区间，具体值报告写明）
2. Entity.mass：f64，serde(default = "default_mass") 默认 1.0；spawn 第 8 参传入，钳制 0.05..=100（超界钳+warn）
3. 阻力定律实现（physics step）：对空中非 aloft 实体（含弹丸）：速度衰减 `v = v - drag_coefficient * v / mass`（方向不变模衰减；线性阻力模型，简单确定性，别上二次阻力）——注意现有普通实体没有速度矢量（它们是 move_by 位置更新制），**阻力 MVP 只作用于弹丸**（唯一有速度矢量的物体）；普通实体物理行为零改动（零回归锚）
4. 现有 .wat（pioneer/example 及测试内嵌 WAT）spawn 调用点补第 8 参 (f64.const 1.0)

### B. 弹丸实体（第一个离体之物）
1. 新实体 kind：`projectile`（core 加枚举值；快照 kind 映射；前端按弹丸渲染——小圆点+尾迹线，颜色=发射者 glyph 色）
2. 弹丸字段：owner EntityId（发射者）/ velocity (vx,vy)（初始速度）/ payload Vec<u8>（携带的攻击字节，同 attack_at 的 payload 语义）/ 寿命（默认 30 拍上限，防永飞——落地的弹丸/超时的弹丸消亡，超时消亡=哑弹 INFO 日志）
3. 弹丸物理（每拍物理步）：
   - 重力：vy += -9.8（吃满，无 aloft 豁免）
   - 阻力：A3 公式
   - 位移：pos += v（一拍一步，与世界的拍节奏一致）
   - 落地（y < radius）：触地消亡（触地判定=落地即结算 payload 于落点——对落点做点命中（#031 判据复用）；打不中任何身体=落地哑弹消亡）
   - **撞身体**：飞行途中每拍与所有活体做碰撞检测（#030 圆判据：弹丸中心距实体中心 < r弹+r实）；命中即结算：对被撞实体走 deliver_attacks 语义（on_attack 防御询问→拦截/换血/击杀掠夺，复用现有管线，弹丸消亡）；**不撞 owner 自己**（出生拍起豁免发射者）
   - 弹丸互撞：MVP 不做（弹丸间穿透，报告注明——将来拦截射弹玩法需要时再立）
4. 弹丸不进沙箱（无 on_tick/life——它是物体不是生物）；不占操控；不可被 probe 锁定（visible 列表里出现但 probe 不可选——报告确认现状行为并注明）

### C. shoot 接口（第 12 个玩家 import）
1. 签名：`shoot(vx: f64, vy: f64, mass: f64, payload_ptr: i32, payload_len: i32) -> i32`（返回弹丸实体序号或 0=失败）
2. 语义：从调用者当前位置发射弹丸（初速度 vx,vy 由代码定；mass 即弹丸质量；payload 同 attack_at 语义——空=默认击杀字节，非空=攻击者的自定义代码字节）
3. 校验：速度模长上限 50（防一拍穿世界）；mass 钳制 0.05..=100；payload ≤1MB（同 attack_at 上限）
4. 每拍发射上限：同实体一拍至多 1 发（防弹幕灌爆——松一点可调，报告写明选定值）
5. api.md 完整文档（含示例 WAT 片段）

### D. attack_at 接触化（贴身刀语义，黑神话棍法）
1. 命中判据从"点在身体内"改为"**出手者与目标身体相触**"：出手者中心到目标中心距离 < r出手 + r目标 + 0.5（接触冗余 0.5 格=胳膊长度常量，写死并文档化——这不是射程属性，是"贴身"的物理判距）
2. 保留点命中寻址：attack_at(x,y) 仍按 #031 找到目标（点在身体内的最近者），但结算前加接触检查——够不着=落空（新落空原因"距离不足"，日志区分于"点不在任何身体内"）
3. **空格操控攻击的适配**：键盘空格攻击最近敌人——若最近敌人不贴身，攻击落空（玩家要学会贴近再砍，物理诚实）；前端点击敌人出刀同语义（点得中但砍不着=落空提示）——**前端加落空反馈**：攻击线变灰+"距离不足"小提示（1 秒）
4. D/A/S/H/P 生物行为适配：D 现在每 4 拍对最近敌人出刀——不贴身全落空会让 D 变废柴；**给 D 的 WAT 加接近逻辑**（不贴身时向目标移动，贴身才出刀）——这是生物配置变更非机制变更，报告列出改动；A 本来就是冲锋近战（天然适配）；S 悬停侦察（probe 不受影响）；H 投机者逻辑核对
5. 两阶段结算/掠夺/on_attack 拦截语义零改动（只是入场多了一道接触门）

### E. 前端（index.html）
1. 弹丸渲染：kind=projectile 的实体画小圆点+速度方向短尾迹；快照字段透传（owner/velocity 可选传，前端渲染够用即可——最小集：位置+kind）
2. 插值：弹丸也插值（同实体管线，无需特判——快照实体数组里就是普通条目）
3. attack_at 落空反馈（D3-3）
4. 命中特效：弹丸撞身体时被撞实体闪白框一拍（前端本地效果，快照驱动）

### F. 文档
- decisions.md：#034 全文（按本任务书开头五条，加"mass 永不参与结算"铁律与"弹丸 MVP 不互撞"边界）
- api.md：shoot 文档 + attack_at 接触语义更新 + spawn 第 8 参
- net-protocol.md v1.8：弹丸 kind=projectile 快照字段 + attack_at 落空原因分类
- progress.md 更新

## 验收标准

1. TDD 红绿全程；675 基线零回归（新增测试净增，预计 +25~40）；clippy/check 三连过
2. Rust 新测试至少覆盖：阻力公式（重弹衰减少/轻弹衰减多/方向不变）；弹丸物理（重力抛物线——固定初速两拍后的位置断言/落地消亡/超时哑弹）；撞身体结算（命中走 on_attack 管线/不撞 owner/贴身接触判据含 0.5 冗余边界）；attack_at 接触化（贴身命中/不贴身落空且日志区分/D 接近逻辑回归）；mass 钳制；shoot 校验（速度上限/每拍一发）
3. CC 起服 headless 冒烟（进程内代）：一段演示——spawn 一个测试生物向 D 发射弹丸（初速朝 D、够近），弹丸两拍命中 D 盾拦截（battle.log 证据链：发射→飞行拍→命中拦截）
4. 土豆浏览器验收：
   a. 弹道可见：需要演示生物发射弹丸——**给 H（搅局者）加 shoot**：每 8 拍向最近敌人发射一发（初速朝目标+一点上抛补偿，mass 0.5）——你能看到黄色 H 抛出的弹丸划弧线飞向目标
   b. 弧线与射程：弹丸飞 5-15 格坠地/命中（拖拽系数调参判据）
   c. 贴身刀：你的 P 离 D 远处点它——灰线+"距离不足"；贴近后点——命中红框
   d. D 的新行为：D 会朝敌人挪（接近逻辑），贴身后出刀
   e. 弹丸命中特效：被撞者闪白
5. 审查聚焦：弹丸结算与现有两阶段管线的挂钩点（无第二结算路径）；接触冗余 0.5 的边界（恰好等于=落空还是命中——定 < 严格并测试锚定）；阻力只作用弹丸的隔离性（普通实体物理零变化的回归锚）；弹丸消亡的所有路径（落地/命中/超时/owner 死亡时在飞弹丸照飞——它已离体归世界）

## 已知信息（不需要查）

- 重力 -9.8/拍；aloft 集合在 physics step 第二参；hover 声明收集在阶段一后
- #031 点命中 find_hit_target 在 main.rs:1417 附近；两阶段 deliver/settle 管线勿动
- #030 碰撞判据 crates/core/src/collision.rs（circle_overlap/point_in_body 都在，复用）
- spawn import 在 crates/arena/src/sandbox.rs（第 7 参 radius 的先例全链路照抄加第 8 参）
- H 生物 WAT 在 main.rs 生物登场处（搜 hunter/搅局者）；D 同（搜 defender）
- 快照序列化 bin/codeworld/src/net.rs（radius/disposition 先例）
- 前端 PURE 区（index.html 434-470 行附近）纯函数 node 验证模式
- D 盾 10/每拦-1；S 替身盾 3；A 冲锋；pioneer 东行建塔
- 世界上限 y=25/x±50（viewport）；弹丸飞出 viewport=超时哑弹路径兜底
- battle.log 是 INFO 级；弹丸飞行每拍不打日志（刷爆），只在 诞生/命中/落地/哑弹 四事件打

## 硬 Gate

```bash
cd D:/projects/CodeWorld && cargo test --workspace
cd D:/projects/CodeWorld && cargo clippy --workspace --tests -- -D warnings
cd D:/projects/CodeWorld && cargo check --workspace
```

## 审查（单审一轮，外部模型）

聚焦见验收 5。另查：#034 文本与实现一致；api.md/net-protocol.md 同步；设计稿（design-ballistics.md）的否决方案没有偷偷复活（弹种表/射程属性/mass 参与结算）。

## 交付物

1. 中文 commit（feat: 期五第三刀——弹道系统#034：阻力+质量+shoot弹丸+attack_at接触化）
2. C:/Users/Administrator/AppData/Local/Temp/cc-report.md（四节：改了什么/怎么验证/边界决策清单/土豆验收步骤）

不要弹选项；信息缺失超3次试错即停写报告。
