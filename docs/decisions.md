# 决策日志

> 纪律：只增不改。改主意 = 写新条目标「取代 #N」，旧条目不删不编辑。
> 每条 ≤5 行，记「为什么」，不记「怎么实现」（实现看代码）。

## #001 2026-10-13 · 攻击语义铁律四条（定了什么算攻击、什么算点数变动）
- 定案：攻击只伤代码（换血/覆写）与算力（防守烧 fuel），不碰生存点数；生存点数唯一变动路径=时间衰减+击杀掠夺；血条只存在于渲染层；掠夺只属于击杀（无吸血式）。
- 原因：堵住"攻击直接扣点数/吸血"这类设计歧路，保住"点数=生存压力、代码=能力"的分层。
- 否决：攻击扣血条方案（数据模型不引入血量概念）。
- 落点：docs/architecture.md 4.2「攻击语义铁律」。

## #002 2026-10-13 · 攻击结算改两阶段模型（收件/结算），取代逐刀即时结算
- 定案：本 tick 全部攻击先收件（逐刀问目标当前沙箱 on_attack，不换血不杀），再按攻击者 EntityId 升序全局定序结算；每刀执行前再查攻防双方存活。
- 原因：逐刀即时结算下"谁被攻击"取决于遍历顺序（随机），不是攻击者的选择；两阶段保证防守者面对全部压力源、攻击执行序确定。
- 否决：按目标分组排序结算（跨组交互会破坏"A 先杀 B 则 B 的刀作废"的验收语义）。
- 落点：main.rs deliver_attacks/settle_attacks；docs/architecture.md 4.2。

## #003 2026-10-13 · 沙箱替换先建后拆（P0 僵尸修复定案）
- 定案：replace_sandbox 先编译新模块、成功才销毁旧沙箱；失败时旧沙箱随 Err 原样返回（Err 载荷 (SandboxHandle, Error)），调用方放回映射表。
- 原因：旧实现先 drop 再 Module::new，非法 payload 编译失败后实体永久丢沙箱成僵尸（存活实体必有沙箱不变量被破坏）。
- 否决：编译前置校验方案（Module::new 跑两遍，编译耗时翻倍）。
- 落点：crates/arena/src/sandbox.rs replace_sandbox。

## #004 2026-10-13 · 换血后 life() 不可测 = 不判死；fuel 继承不缩水配额（审查定案）
- 定案：call_life Err 一律按架构 4.2 默认行为处理（life 维持原值，无击杀无掠夺），废除"破损沙箱等同死亡"；fuel 继承只继承当 tick 剩余量，fuel_limit 恢复原配额。
- 原因：防防守 fuel 被磨穿时良性击晕刀误收割击杀掠夺；防反复被攻击单调磨死绕过"超额不死亡"保护。
- 否决：区分 fuel 耗尽与模块破损的判据方案（wasm 内 out-of-fuel trap 在 Windows 会进程 abort，无法安全到达该分支，见 .cc-bugs #5）。
- 落点：bin/codeworld/src/main.rs replace_code；docs/architecture.md 4.2 技术保护。

## #005 2026-10-13 · 战况字幕=渲染层展示，盾量走玩家 env.log 自愿上报（零新 import）
- 定案：字幕写终端第 1 行（真实视口下 row 0=世界 y=-25 地下空白区，不占战场）；盾量等沙箱内部状态由玩家代码 log marker 自愿上报（D 拦截时 log(7)+log(余量)），host 解析 SandboxData.log_markers 序列；距离/存活数取 world 快照。字幕状态变化才打 info 日志（tick 不算变化）。
- 原因：不为字幕破坏沙箱隔离（host 读不到 WAT global 是特性不是缺陷）；零新机制复用既有 log 通路。
- 否决：新 env import report(v)（多一条 import 通路，与 log 语义重复）；host 直读沙箱 global（破坏隔离）。
- 落点：crates/render/src/caption.rs；crates/arena/src/sandbox.rs log_markers；main.rs parse_shield_markers + tick d2 步。

## #006 2026-10-13 · 字幕/canvas 文案全 ASCII（GBK 代码页免疫）
- 定案：终端画布上的字幕用 ASCII（"tick=12 | A->D dist=2.4 | D shield=7 | alive=3"），不用任务书样例的中文字段名。
- 原因：Windows cmd 默认 GBK 代码页，Rust 输出 UTF-8 中文会乱码（bat 禁中文同源教训）；ASCII 每字符恰一格，宽度可预期。
- 否决：chcp 65001 切码页方案（用户终端环境不可控）。
- 落点：crates/render/src/caption.rs format_caption；start.bat/start-reversal.bat。

## #007 2026-10-13 · H 搅局者默认登场 + CODEWORLD_H_ENABLED 开关；性格用 tick 奇偶当伪随机
- 定案：H（黄"H"@(10,8) 投机者）默认登场（start.bat=混战主秀），CODEWORLD_H_ENABLED=0/false 关闭回经典双生物对局；50% 拦截用 tick 奇偶（WAT 无随机源），游走方向同理。
- 原因：三剧本冒烟（默认 D 赢/翻盘 A 赢/混战）需要可切换的双生物模式；start-reversal.bat 关 H 保证薄盾翻盘剧情可见（H 挡在 A 冲锋路线上会先杀 A）。
- 否决：H 固定登场不可关（混战与翻盘剧本互斥，无法分别冒烟）。
- 落点：config.rs h_enabled；main.rs HUNTER_WAT/spawn_test_entities；start-reversal.bat。

## #008 2026-10-14 · probe 坐标寻址 + ProbeBridge raw 指针桥 + 体徵尾块（三层感知 Phase A 定案）
- 定案：MVP probe 用坐标寻址（tx,ty 代替 EntityId，查找限定可见快照）；import 闭包经 ProbeBridge（挂到 SandboxData 的 raw 指针，阶段一窗口挂载/置空）同步访问目标沙箱；被动体徵固定为 response 后 24 字节（api_calls 计数/fuel 剩余/on_probe 消耗，u64 LE）；被探测方 on_probe 用 host 侧 set_fuel 独立预算包裹后恢复。
- 原因：import 闭包是 'static 拿不到 host 引用而 probe 必须同步回响应（排队模式玩家拿不到数据）；体徵定长尾块让玩家代码可解析、撒谎串与真实体徵同框可比对；独立预算防恶意 on_probe 烧目标 tick 配额（probe 不扣手续费，架构 3.3）。
- 否决：EntityId 传递（MVP 玩家代码持有坐标即可，传递要跨沙箱共享 id 类型）；probe 排队延迟响应（破坏"探测后决策"玩法）；epoch interruption（MVP 改造成本大，且 fuel trap=abort 是 .cc-bugs #5 环境级已知）。
- 落点：crates/arena/src/sandbox.rs ProbeBridge + env.probe/visible_count/visible_at；main.rs fill_visible_snapshots/run_player_code 挂桥；docs/architecture.md 3.2 MVP 落地差异。

## #009 2026-10-14 · S 出生位 (25,12)（微调任务书建议 (20,5)）+ D 反制前 probe 看一眼
- 定案：S 出生 (25,12) 右侧高台；D 的 on_tick 反制刀前对 scan 坐标 probe 一次（只读、结果不用于决策）。
- 原因：D 的 scan 锁最近敌人，S@(20,5) 距 D 7.07 会抢在 A(10)/H(9.4) 前被反制点名，tick4 即死破坏剧本 1-3 回归；(25,12) 距 15.6 排最远不抢锁。四方里只有 S 有 on_probe，无人探测则 LIE9 撒谎串永不见天日——D probe 一次让"probe S 拿到假串+真实体徵"在实战日志可验证，probe 只读不改变 D 的打谁何时打（回归行为不变）。
- 否决：改 A/H 的 WAT 加 probe（动作面更大）；S 侦察两目标（复杂化）；接受撒谎只存在于单元测试（剧本 4 验收要求实战日志断言）。
- 落点：main.rs spawn_test_entities/defender_wat；[probe] 日志新增 resp_head 摘要（0x3945494C 可直接 grep）。

## #010 2026-10-14 · S 悬浮=玩家代码每拍抵消重力（补充 #009："高台"从叙事变物理事实）
- 定案：S 的 WAT on_tick 在侦察节奏判断之前每拍 move_by(0,+9.8)（生产重力=DefaultPhysicsEngine::new() 默认 -9.8/拍直接改坐标，非速度累积）；阶段二 a 先应用悬浮位移、物理步再扣回，净悬停 y≈12。悬浮位移兼作防御：阶段一快照坐标与 a2 攻击结算坐标恒差 9.8，反制刀精确坐标匹配永远落空。
- 原因：#009 的"右侧高台"只是叙事、物理上不存在，S 第一拍即坠地被 D 无射程上限的反制刀点杀；按能力/规则分层，悬浮是玩家代码的规则不是框架地形能力，不建地形系统。
- 否决：条件悬浮（y<12 才 +5——结算/物理顺序下会振荡落到 y≈7）；框架给实体加 fly 标记（超出 MVP 能力面）；建地形系统（本任务明确排除）。
- 落点：main.rs SPY_WAT（move_by import + 每拍悬浮调用）+ spy_hovers_above_ground_every_tick 测试。

## #012 2026-10-14 · 一个基座、两扇门、三张合同（存在性体系定案）
- 定案：实体是唯一原语，能力全统一（含造物再造造物）；区别只在进场门（认证进场 vs 代码spawn）与存在合同（在线锚/自治锚/连接锚）。角色=在线锚合同的特殊造物。合同可组合归玩家，框架只强制战斗残影期5拍（#013）。全文 docs/world-foundation.md。
- 原因：土豆发现"玩家接口与造物接口能力大量重复"——重复即同构，合一套系统；能力归框架、玩法归玩家的宪法推到存在性本身。

## #013 2026-10-14 · 战斗残影期：最近30拍参战者下线一律残影5拍，不可豁免
- 定案：防拔线逃课（死亡=掠夺的世界里临死拔线=免费保命）。行业先例：H1Z1断线残留可被杀、EVE安全下线读条。

## #014 2026-10-14 · 外部通道=通用能力：实体声明创造者端点，世界只转发，断连降级本体逻辑
- 定案：NPC接大模型/阵法接算力/城接实时指挥=一个接口三种用法。付费归创造者（氪金生态位）。延迟即拟人（当拍保底话术+数拍后AI补充）。

## #015 2026-10-14 · 服务挂载位置=建造者自由：挂城墙（人死城在，可攻占继承）或挂灵魂（人死城毁，焦土）
- 定案：框架不裁决，产权链记录归属，攻占=覆写核心建筑代码（夺舍既有机制）。

## #016 2026-10-14 · 残影=完全惰性躯壳（on_tick/life/on_attack 全停），消散时刻=检测拍+时长拍
- 定案：残影期不执行任何玩家代码（防守询问也跳过——拔线者不能继续享受原代码拦截）；消散=标记死亡走既有清理通路（无掠夺）；残影实体先被杀=到期静默降级。probe 残影仍答 on_probe 是已知 MVP 缺口（桥不感知残影）。
- 原因："无人躯壳"的诚实语义；防守若照跑等于拔线玩家继续操控角色受益。
- 否决：残影保留防守（违反惰性语义）；probe 桥加残影感知（MVP 改造成本大）。
- 落点：main.rs run_player_code 跳过 + deliver_attacks 防守跳过 + step_online_anchor；docs/api.md 三节。

## #017 2026-10-14 · 出刀即战斗（审查 P1 修正）：落空刀也记参战
- 定案：deliver_attacks 攻击者存活检查通过即记参战（落空/拦截/命中都算），不只命中收件才记。
- 原因：堵"对空出刀再拔线"绕过 #013 战斗残影的漏洞；api.md"出刀或挨打都算"与实现对齐。
- 落点：main.rs deliver_attacks record_combat_id；测试 blade_miss_still_counts_as_combat_for_ghost。

## #018 2026-10-14 · 超视距情报铁律：框架零供给，玩家自构建
- 定案：框架不提供任何超视距获取坐标的能力。超视距情报=玩家自行构建：规则逆向（概率圈）、运动建模（指纹外推）、哨兵网络（造物视野拼接）、侧信道（间接效应侦击）。攻击落空可作为排空信息（声呐扫图合法，受暴露/燃料成本自然制衡）。0比特墙=未接触实体的存在本身无信息位，精确坐标理论不可推。
- 原因：土豆问"有无推演手段获得未接触实体坐标"——定性为世界宪法级。眼睛诚实、墙外靠自己，与藏坐标/破坐标军备竞赛同构。哨兵网络=期二门二spawn的首个应用场景。
