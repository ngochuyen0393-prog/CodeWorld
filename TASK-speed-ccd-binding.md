# TASK（期五第四刀·小刀四件套）：世界极速#034修订 + CCD路径检查 + 发射痕迹 + 绑定契约原语

## ⚠️ 必须全程 TDD（每个行为先红后绿）；四件互相独立可分批 commit，但一刀交付

## 宪法依据（本刀修订 #034 + 新增条款，写进 decisions.md）

### #034 修订一：世界极速（视觉可读性常数）
土豆三天推演收敛的立法（原话定则："速度上限与拍速有关，保证每拍物理规则生效"→修正为视觉可读性常数；"必须有一个上限，画面要美，光速怪会让可玩性消失——这应该是所有游戏都不允许存在的情况"）：
- **世界极速 = 25 格/拍**，立法身份：视觉可读性常数（telegraphing constant）——任何离体之物一拍位移的硬上限，理由：一拍最大位移不超过世界宽度 1/4，任何飞行物在画面上至少存在多拍、肉眼可追随，攻击永远"看得见飞过来"
- 不是玩法平衡数字，是世界的物理常数（与重力 -9.8 同级）；同时天然保证防御反应窗口（极速内任何弹至少飞 1 拍以上，零窗口狙击物理上不存在）
- config 可调：CODEWORLD_MAX_SPEED（默认 25.0）
- **标定锚**：横穿全图（100格）最短 ≈4 拍的观感；**拍速变更时随世界常数批次重标定**（不自动公式耦合，明改明公告——拍速升级是已排期的独立大件，届时重力/阻力/极速一起重标）
- 实施范围：发射时钳制（现状 MAX_SHOOT_SPEED=50 退役）+ **飞行中钳制**（修双审 P1-A：重力累积速度不封顶的洞）

### #034 修订二：CCD 路径检查（防穿透，工程责任）
- 弹丸撞身体判定从"点检查"（只看拍末位置）升级为"路径检查"：一拍内从起点到终点的线段扫过任何身体（点到线段距离 < r弹+r身）即命中
- 修双审指出的隧穿（高速弹一拍跳过目标）；极速常数之内的任何速度都如实结算
- 业界标准（Box2D bullet/Unity continuous 同款）；点到线段距离一个公式，2D 圆碰撞不贵
- 命中多个身体时取路径上最先碰到的（线段参数 t 最小者），定序消灭随机性

### #034 修订三：发射痕迹（发射即暴露）
土豆定则："我们还要考虑可玩性，光速怪谁也看不到对方"；调研 EVE 锁定链/战地弹道可读后定的世界规则：
- shoot 的瞬间，发射点在世界里留下**发射痕迹**（一拍寿命的信号实体/标记）
- 可探测性：痕迹在 visible 列表出现（一拍）；probe 命中痕迹坐标时返回发射者的身份信息（resp 携带"此处发射过弹丸"）
- 痕迹强度随弹丸动能（速度×质量）增大：强弹痕迹寿命更长（1~3 拍，封顶）
- 语义：攻击自由，但攻击留下痕迹——反制手段（哨塔网/痕迹追踪反击）玩家自己长；世界只保证"打了就有记录"
- 快照可选携带痕迹（前端渲染淡淡的发射闪光一拍）——最小实现：痕迹不进快照，仅 probe/visible 可见也可（CC 自行评估最小集，报告写明取舍）

### #034 修订四（新条款号或并入 #034 注记，CC 判断）：绑定契约原语
土豆定则（design-binding.md 已存档）："绑定契约、活弹这一类机制型的东西才是我们要做的"——本刀落地契约 MVP，活弹另刀：
- **shoot 加第 6 参 binding（i32 枚举）**：0=unbound（默认，现状：主人死弹照飞+命中按现行"死者攻击作废"结算）；1=fade_on_death（同生：主人死→在飞弹丸即刻消亡，哑弹日志）
- **spawn 加 binding 同款**（造物契约：0=unbound 塔现状独立存在；1=fade_on_death 主人死塔消亡）——同生不共死=unbound 现状默认，本刀即契约化的第一步
- **攻击豁免条款**（P1-B 定案）：binding 第 3 值 2=grace_owner（弹丸全程不伤发射者，含飞行撞体与落地结算两路径——落地路径现状不豁免是双审 P1-B 抓出的不对称，本定案：世界提供声明式豁免，玩家不声明则落地砸自己合法——"玩家完全可以给自己的攻击代码做成识别的，世界不豁免，豁免是玩家的代码能力"（土豆），但 MVP 先给声明开关，手写 on_attack 识别仍是完全体路径）
- 契约=出生时声明写进造物数据，世界结算引擎每拍托管执行（控制vs绑定的区分：离体无链路故无控制，契约不需要链路——遗嘱模式）
- api.md/net-protocol.md 同步文档

## 项目上下文

项目：D:/projects/CodeWorld（基线 719 测试全绿，git 2454428）
- 弹道系统已落地（6ff4527）：弹丸实体/物理步/两阶段结算挂钩/attack_at 接触化/H 演示抛射
- MAX_SHOOT_SPEED=50 现状在 sandbox.rs import 层（本刀退役换极速常量）
- 物理步 step_projectiles 在 crates/physics/src/default.rs:195 附近（P1-A 洞：积分后无速度钳制）
- 点命中 find_hit_target main.rs:1417；圆判据 collision.rs（circle_overlap/point_in_body）
- shoot 签名：(vx, vy, mass, payload_ptr, payload_len) -> i32（本刀加 binding 第 6 参）
- spawn 签名 8 参（本刀加 binding 第 9 参）——WAT 调用点同步（pioneer/测试内嵌 WAT）
- 双审遗留 P2 清单（本刀顺手修前两条）：落地弹尸体坐标未钳回半径线；shoot 返回排队序号但阶段二可能拒绝（改文档注明"排队序号非落地保证"）
- design-binding.md（绑定设计定案）/design-ballistics.md（弹道定案）——细节以文档为准

## 需求（四件互相独立，每件独立红绿）

### A. 世界极速（Rust：core/physics + arena/sandbox + config）
1. config：max_speed（CODEWORLD_MAX_SPEED，默认 25.0，校验 >0 有限）
2. MAX_SHOOT_SPEED=50 退役：import 层改用 config.max_speed 钳制初速（超限拒绝返回 0，warn 日志）
3. **飞行中钳制**（修 P1-A）：step_projectiles 积分后（重力+阻力+位移前）速度模长 > max_speed 则归一化压回——重力累积从此封顶
4. 测试：初速超极速被拒；重力累积 30 拍后速度 ≤ max_speed（重弹 mass=100 场景——P1-A 复现锚）；极速恰等于时不拒（边界 < ）

### B. CCD 路径检查（Rust：physics step_projectiles 撞体判定）
1. 撞身体判定：从"拍末点在身内"升级为"起点→终点线段与身体圆相交"（点到线段距离 < r弹+r身；造物 AABB 用线段-AABB 相交）
2. 一拍路径命中多个：取线段参数 t 最小者（最先碰到）
3. 落地判定同样升级：路径先触地则结算于触地点（落地弹的落点=线段与地面线交点，不再是拍末位置）——顺手修 P2"落地弹尸体坐标"相关问题
4. 测试：高速弹（极速 25）一拍跨 20 格、目标在路径中点——必命中（点检查会漏的锚定用例）；多目标取最近；贴地飞行弹先触地结算

### C. 发射痕迹（Rust：main/physics + arena probe）
1. shoot 成功发射时：出生点记录痕迹（实体账本或独立标记表——痕迹不是实体不占碰撞/不进沙箱，最小实现自由，报告写明结构）
2. 痕迹寿命：1 拍基础 + 动能加成（速度×质量超阈值 +2 拍，封顶 3 拍）
3. 可探测：visible 列表包含痕迹（一拍）；probe 命中痕迹坐标返回发射者 id + 拍号（resp 格式进 api.md）
4. 日志：发射 INFO"[shoot] 弹丸发射+痕迹留下 owner=… 位置=… 动能=…"（现弹丸诞生日志合并或并存，CC 定）
5. 测试：发射后痕迹可见一拍；强弹痕迹 3 拍；probe 返回发射者

### D. 绑定契约（Rust：arena/shoot+spawn 签名 + main 结算 + core）
1. shoot 第 6 参 binding i32：0 unbound（默认）/1 fade_on_death/2 grace_owner；非法值钳 0 warn
2. spawn 第 9 参 binding 同款（造物契约：1=主人死塔标记消亡——死亡清理链检查所有造物契约）
3. fade_on_death 执行：每拍结算检查 owner 存活，owner 死→弹丸/造物消亡（INFO"[binding] 同生契约生效，随主消亡"）
4. grace_owner 执行：弹丸撞体判定与落地结算两路径都跳过 owner（落地不砸自己——声明式豁免）；unbound 落地砸自己合法（P1-B 定案：世界不豁免，豁免是声明）
5. 死者弹丸语义现状保持（unbound 主人死后照飞、命中按"攻击者已死作废"结算）——但 fade_on_death 弹根本活不到结算（主人死即消散），两条路径在报告里写清
6. WAT 调用点（pioneer/内嵌测试）spawn 补第 9 参 0；shoot 现调用点（H 的抛射）补 binding 0
7. 测试：fade_on_death 主人死弹消亡（时序：主人死那一拍弹即无）；grace_owner 落地弹不结算发射者；塔 fade_on_death 随主人死消亡；unbound 全部现状回归锚

### E. 文档
- decisions.md：#034 修订四条全文（极速常数/CCD/发射痕迹/绑定契约 MVP）——修订格式按项目惯例（#034 原条目下追加修订记录或新条目，CC 按 decisions.md 现有格式惯例定）
- api.md：shoot 第 6 参/spawn 第 9 参/binding 枚举/痕迹 probe 返回
- net-protocol.md：如痕迹进快照则 v1.9，不进则只更新语义节
- progress.md 更新

## 验收标准

1. TDD 红绿全程；719 基线零回归（净增预计 +15~25）；clippy/check 三连过
2. 双审 P1-A 修复验证：重弹自由落体 30 拍速度曲线封顶于 max_speed（测试锚）
3. CCD 锚定用例：中点目标高速命中（旧点检查必漏）
4. headless 冒烟（进程内代）：发射 fade_on_death 弹→击杀发射者→弹当拍消亡；发射 grace_owner 上抛弹→落回脚下→不炸自己（battle.log 证据）
5. 土豆浏览器验收：
   a. H 的抛射照常（binding 0 回归）
   b. 极速内弹道观感不变（25 上限对现 H 弹无影响——它的初速远低于此）
   c. （可选）临时给 H 换 binding=1 验证同生消亡——CC 若沙箱起不了服则进程内测试代
6. 审查聚焦：极速钳制的两处（发射/飞行）一致；CCD 线段判定与落地判定顺序（先撞体后落地）；痕迹最小结构不膨胀成实体系统；binding 参数链路（WAT/arena/main）三处同步；死者弹丸两路径语义文档清晰

## 已知信息（不需要查）

- H 抛射 WAT 在 main.rs 生物登场处（搜 hunter）；现 shoot 调用点无 binding 参数
- 弹丸账本 EntitySandboxMap.projectiles（owner/payload 在账本，Entity 存物理）
- 死亡清理链在 main.rs tick 流程（造物契约检查挂这里）
- 物理步顺序：弹道步（撞体/落地事件）→ 位置重力步（普通实体）——CCD 改的是弹道步内部
- probe 现返回 resp+体徵（sandbox 侧 bridge）——痕迹 probe 走最小路径（不建沙箱，直接返回构造 resp）
- 世界上限 y=25/x±50；地面 y=0
- design-binding.md 第五节排期：本刀=①绑定契约小刀；②活弹+③外接桥另刀；④弹弹相撞另刀

## 硬 Gate

```bash
cd D:/projects/CodeWorld && cargo test --workspace
cd D:/projects/CodeWorld && cargo clippy --workspace --tests -- -D warnings
cd D:/projects/CodeWorld && cargo check --workspace
```

## 审查（单审一轮）

聚焦见验收 6。另查：#034 修订文本与实现一致；api.md 三处签名同步；极速常数的标定锚（全图≈4拍）写进注释/文档；P2 顺手修的两条真实修掉。

## 交付物

1. 中文 commit（可分件多个 commit，最后统一 feat: 期五第四刀——世界极速+CCD+发射痕迹+绑定契约#034修订）
2. C:/Users/Administrator/AppData/Local/Temp/cc-report.md（四节：改了什么/怎么验证/边界决策/土豆验收步骤）

不要弹选项；信息缺失超3次试错即停写报告。
