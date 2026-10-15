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

## #019 2026-10-14 · scan 视野化定案：眼睛诚实，架构 4.2 三路前置条件完整兑现
- 定案：find_nearest_enemy 加 viewport（BoundingBox）过滤，与 fill_visible_snapshots 共用 tick 内同一个渲染视口实例（同源口径）；视野内无目标 scan 返 0。D/A/H 的 WAT 本就有 scan=0 跳刀/待机分支，无 WAT 改动。屏幕内=看得见=合法目标（D@15 距玩家出生带 15-30 格仍在默认视口 [-50,50]x[-25,25] 内照常点杀，这是边界效应不是缺陷）。
- 原因：scan 原是"上帝视角单敌人"（代码注释自认），违背架构 4.2 攻击前置条件三条路（视距内/接触式/超视距）——眼睛层 visible_count 已 viewport 过滤而 scan 没有，两套口径；#018 铁律定了框架零供给，scan 供给超视距坐标直接违宪。
- 落点：main.rs find_nearest_enemy/run_player_code/tick；sandbox.rs nearest_enemy 注释与 scan 无目标日志；api.md scan 条目。

## #020 2026-10-14 · 门二开放定案：spawn ±10 格 / 64 上限 / 产权链 owner；destroy 仅限自家造物 MVP
- 定案：env.spawn 出生点限创造者快照 ±10 格（每轴，MVP 一刀切防远程投射刷屏，玩法化留宪法）；世界实体上限 64（CODEWORLD_MAX_ENTITIES）；每实体每拍限 1 个请求；造物 owner 继承创造者 owner（产权链 #015）、fuel 继承、code_hash="creation"、自治锚存续。env.destroy 只开放"毁自家造物"（阶段二比对 owner 匹配），无掠夺（不是击杀）；击杀式销毁走攻击通路。
- 原因：±10 用 host 填充的快照坐标在 import 层校验（同 move_by 模式），玩家伪造不了；上限/编译失败放阶段二（import 拿不到 world）。destroy 与攻击分流：掠夺语义只属于击杀（铁律 #001），产权销毁是所有权处分不是战斗。
- 否决：EntityId 传 spawn（造物 id 由框架生成）；import 层做 owner 校验（沙箱拿不到 world）；造物继承创造者坐标而非自选（削弱玩法面，±10 已够防刷）。
- 落点：crates/arena/src/sandbox.rs env.spawn/env.destroy + PendingSpawn/PendingDestroy；bin/codeworld/src/main.rs process_spawns/process_destroys/tick a 与 a1 步；config.max_entities；examples/sentinel.wat；docs/api.md 门二两节。

## #021 2026-10-14 · 实体占位制定案：一格一物（Screeps 阻挡制起步，半径制留路线）；推挤机制退役
- 定案：占位判定=格子口径（floor(x),floor(y)，一格一物），移动/重力地面/spawn 三处同源过 codeworld_core::occupancy::cell_occupied_by（本拍移动按 EntityId 升序先到先得，确定序）。出生格被占（含创造者自己的格，无自豁免）→ spawn 失败可重试；落被占格 → 悬停上一格下拍重试。推挤（resolve_collision）退役：tick 流程不再调用，同格只记 warn 不推挤（异常可见不自动修复）。接触语义扩为两类：物理同格+移动被挡，逐对 info 日志（为接触开通道铺路）。
- 原因：实测 P/T 连体推挤 bug 实锤（挤压式解决=永久连体环游世界，Factorio/EVE 社区公认反面模式）。调研四流派：Minecraft/Wurm 证明格子制撑得起 3D；EVE 证明半径+服务器裁决；Screeps 阻挡制最简可玩（MVP 起步）；原神互穿模型不适用（玩家非对抗，身体阻挡是世界乐趣的一部分）。半径制留作将来路线：体型开放时四件套（上下限/面积付费/总预算/半径判定）同天上线，接口形状已按格键隔离预留。
- 否决：推挤+分级体量（连体 bug 反面模式）；方向禁入（进格后才回弹，闪烁难看且裁决仍需排序）；spawn 落占格时自动找邻格（世界擅自改玩家意图，应失败让玩家自选）。
- 落点：crates/core/src/occupancy.rs cell_occupied_by；crates/physics/src/default.rs step 占位重力+推挤退役（resolve_collision 标 deprecated 留档）；bin/codeworld/src/main.rs apply_pending_changes 移动裁决+ContactPair/handle_collisions 逐对日志+process_spawns 出生预检；players/pioneer.wat 塔改 +5+立塔拍不移动。

## #022 2026-10-14 · 双层玩家模型定案（代码层=角色底层，操控层=实时参与）
- 定案：代码层（存在/形态/强度/行为逻辑=玩家代码，离线持续运转，三态合同不变）+操控层（鼠标键盘实时操控：点地走路/探索/战斗决策=在线参与）。输入=意图流（非坐标直写，防瞬移，物理照常裁决），经角色代码解释执行，代码可 override 意识（自动闪避优先于操控走位=代码强的直观体验）。操控通道=#014 外部通道首个应用（阵法接算力/NPC接大模型/玩家接鼠标三位一体同构）。离线=无输入=回落代码自主（自主逻辑=挂机AI）。
- 原因：土豆拍板"探索世界要玩家亲自参与，代码是背后的事"——纯 Screeps 模式砍掉 99% 不会写代码的玩家；双层模型拆开门槛（不会代码=用模板纯操控，会代码=底层更强），代码交易自然成立（好代码=好装备）。攻击=覆写在新模型下语义升级：夺的是"角色基因"，操控权仍在玩家。
- 修正：此前"P被塔挡只会撞"不是bug是代码层题——但新模型下玩家直接鼠标绕行，代码寻路是加分项非门槛。Phase 3（WebSocket+Canvas+在线IDE+输入）从"多人网络"重命名为"操控层"，优先级提前候选。

## #023 2026-10-15 · 操控层v0定案：crossterm意图队列+操控覆盖自主+物理零豁免；路由独立成层留期四复用
- 定案：键盘走 crossterm raw mode（roguelike 事实标准）+独立输入线程+mpsc 意图队列（tick 每拍 drain）；操控目标=players/ 第一个进场玩家（单玩家 MVP）；按键拍跳过该实体 on_tick（完全接管，life() 照常=存在合同不受影响），操控移动/攻击只排队进既有占位裁决与两阶段结算——操控零豁免物理；同拍多键后到覆盖先到；字幕 ctrl:字形+被操控实体白色高亮（渲染层特判不动 core）；route_intents 独立纯函数（main 只喂最近敌人坐标），期四 WebSocket 只换生产者不换路由。
- 原因：键位/回合排队/视野攻击抄 roguelike 成熟语义不发明新规则；"按键拍完全接管"是最简可证语义（部分接管需标注 on_tick 哪些副作用豁免，复杂且挂机AI收益未证实）；入口独立是为 #014 外部通道三位一体（键盘/WS/未来端点同构）留的形状。
- 否决：坐标直写（违 #022 意图流定案）；操控期间保留自主移动（双移动源同拍冲突，裁决复杂化）；高亮跟随按键瞬闪（回合制下高亮闪现丢角色，roguelike 惯例=常驻可辨）。
- 落点：bin/codeworld/src/input.rs（Intent/key_to_intent/route_intents/输入线程/终端进出恢复+panic hook）；main.rs tick_with_intents；render caption ctrl 段+white 高亮；api.md 操控层 v0 节。

## #024 2026-10-15 · 操控身份绑定定案：操控权=me声明非进场序；下线latch观战不自动恢复
- 定案：操控目标=CODEWORLD_ME（优先）或 players/me.txt 声明的名字且已进场在线（每拍解析，中途声明/修改即生效）；未声明/声明无效=观战（按键仅 Quit，字幕 ctrl:--，无高亮）；操控目标下线 warn 一次并 latch 观战，重新进场不自动恢复（重启重开）；example.wat 归位 _template.wat.txt（下划线前缀+非 .wat 扩展名不进场）。修正 #023"第一个进场玩家"偷懒语义——操控权=身份绑定（终态句式"输入→我的角色"），期四凭证升级（文件声明→账号认证）只换声明来源，路由不变。
- 原因：进程序自动轮替会把键盘送给"碰巧先进场的陌生人"；下线=会话断开，自动重连恢复操控给"断线抢尸"留歧义，MVP 从简显式重启。
- 否决：保留 first_online 进场序语义（#023 修正对象）；下线后同拍自动恢复操控（会话语义模糊，动态恢复留下个任务）；me.txt 加 mtime 缓存（一行文件每拍读成本可忽略，缓存是无谓状态）。
- 落点：players.rs read_me_declaration/controlled_target（first_online+online_order 退役）；main.rs tick ②5 三态解析+下线 latch+ctrl:--；players/_template.wat.txt；start.bat/players/README.md/api.md 同步。

## #025 2026-10-15 · 操控独占与下线处置权定案
- 定案：在线（me 声明有效）=操控独占——玩家不按键角色就站着不动，代码自主被完全抑制，不存在"超时交还"（否决 5 拍窗口方案，方向反了）；下线=玩家处置权：A 带走（残影消失，默认）/ B 停留世界+选接管者（自己写的后台逻辑=自主模式，或外部 AI=连接锚 #014）。接管者与键盘共享意图路由入口（input.rs 单入口双来源）。手感细节（按住连走/刀感节奏）推迟到渲染画面阶段（期四）再调。
- 原因：土豆拍板"上线状态不允许后台接管，只有登出后自己选择停留+接管方式"。实施与期四浏览器化强耦合（选择界面/AI接入），攒到期四一次成型，当前不阻塞验证。

## #026 2026-10-15 · 期四浏览器化定案：薄客户端/快照推送 v1（第一刀）
- 定案：世界唯一权威，浏览器=显示器+遥控器。第一刀只做"推"：每拍全量 JSON 快照（tick/caption/viewport/entities，≤64 实体几 KB）经 tokio broadcast 从 std 主循环桥到独立 ws-server 线程（axum 0.7，/ws 快照广播 + ServeDir 静态页一把抓，绑定 127.0.0.1）；kind 映射 code_hash：四生物=npc / creation=creation / 在线名或残影=player；player 恒带 name+controlled，creation 带 owner，npc 无可选字段。插值渲染（第二刀）/输入回传（第三刀）/预测（第四刀）后续。
- 原因：快照先对再省（delta/二进制留以后，前端要的是字段稳定不是带宽）；broadcast Sender::send 非 async 天然跨线程，std tick 侧零 async 改造；WS 线程挂/端口占用只 warn，终端世界完全不受影响；绑回环免防火墙弹窗，MVP 不暴露公网。
- 否决：tokio-tungstenite 裸写服务器（静态伺服要再手搓 HTTP，axum 一把抓）；控制流反向（WS 线程拉快照改推模式，主循环耦合 async）。
- 落点：bin/codeworld/src/net.rs；docs/net-protocol.md v1 前端契约；config.ws_port（0=禁用）。

## #027 2026-10-15 · 鼠标操控定案：点击走位/点击出刀/操控独占#025落地（第三刀）
- 定案：上行三消息 move/attack/stop（net-protocol.md v1.2）经 axum WS 接收循环解析入 input.rs 意图队列（IntentSink Clone 共享给 WS 线程——终端键盘与浏览器鼠标单入口双来源，路由不分来源）；move=持续目标逐拍先横后竖走一格、格距≤0.5 到达清除、键盘接管清除、attack 不清目标（单刀与走位并存）；点击出刀不判距离（落空自担，玩法归玩家）、与键盘空格同一两阶段结算；单连接假设=第一 WS 连接认领操控令牌（AtomicBool）、断开释放、观战连接上行丢弃——"本机浏览器=我"是 me.txt 的连接版等价物，意图作用仍由 me 声明裁决（连接≠操控权）；#025 操控独占同步落地：me 在线即全程抑制操控目标 on_tick（不再按 action 有无回落自主），状态翻转沿打"[ctrl] 操控独占开始/释放"日志；前端左键点地=move/点敌人=attack（命中=快照最近实体距点击<半格，排除自己）、右键/S 键=stop、走位虚线+目标标记+出刀红框闪（全本地提示）。
- 原因：业界标准 client input → server authority（SnapNet/netcode 惯例，客户端只发意图服务器全权裁决，预测留第四刀）；单入口路由是 #023/#014 的既定形状（外部通道三位一体）；单连接假设是 me.txt 身份模型在本机的最小推广（多人认证留后）；操控独占修正了 #022 时代"无意图拍回落自主"的临时语义（土豆 #025 拍板：在线不许后台接管）。
- 否决：攻击距离预判+前端提示（加规则=框架管玩法，违宪法；落空结算已是自然反馈）；点击敌人自动走近（同类）；多操控连接/连接即身份（MVP 单人本机，认证化是 #024 终态议题）；Intent 加来源字段（路由不分来源=最简，排错靠日志即可）。
- 落点：input.rs Intent 三变体+route_intents 五参折叠；net.rs ClientMessage 解析+WsState 操控令牌+转发；main.rs tick_with_intents 独占抑制+ws_move_target 状态；static/index.html 点击/右键/S 键+虚线红框；net-protocol.md v1.2；f64::signum(+0.0)=1.0 坑记入 next_move_step 注释。

## #028 2026-10-15 · 期四第四刀手感定案：预测镜像规则是协议契约入宪法；镜头/刀光是表现层不入
- 定案：Valve 标准组合"己方预测+他人插值"——浏览器对自己发的 move/stop 建预测（锚点=最新快照 P 服务器坐标、拍号+1，进行中的一拍乐观先走一步=点击瞬时起步）；每帧快照对账阈值 0.25 格，一致锚点前移、分歧（被挡/外力/观战连接）瞬移回服务器真实坐标并清本地走位目标；P 死亡/断线重连清全部预测态；键盘（终端）来源不预测。预测规则= input.rs next_move_step 的 JS 镜像（applyMoveSteps），**改一处必须同步另一处**，契约锚在 net-protocol.md v1.3「客户端预测与和解」节。Rust 侧仅日志分级：apply_pending_changes 的 SetPos 应用日志仅 control_target 升 INFO 带 from→to（其余保持 debug 防刷爆）。
- 原因：1秒/拍+1.5拍插值延迟下预测是消除"点击后 2 秒才动"的唯一正解；镜像规则是双实现漂移风险点，必须文档钉死而非口头约定；镜头跟随（lerp+viewport clamp+F 切换）、刀光线、击杀 ✕ 属纯表现层可随时重调重做，入宪法只会锁死调参空间。
- 落点：bin/codeworld/static/index.html（PURE 区镜像函数+reconcile/startPrediction/stopPrediction+镜头 lerp/clamp/边界线）；main.rs apply_pending_changes 日志分级（14 处测试调用点补 None 参数，无断言变更）；net-protocol.md v1.3 §7。

## #029 2026-10-15 · 算力概念宪法修正：fuel=纯工程保险丝无玩法意义；算力=造物的外接资源位
- 定案：两层正名。① sandbox fuel（每拍重置的指令预算）是防玩家代码死循环卡死世界的**工程保险丝**——正常代码永远碰不到，无任何玩法意义，禁止在任何玩法/收费/代价设计里引用它（推翻此前"悬停折寿/用生存时间换制空权"的错误提案——那是把保险丝错当生命值，为难度而难度）；生命资源只有生存点数（每日衰减+击杀掠夺，#001-004 既定）。② 真正的"算力"是**造物的外接资源位**：飞行/瞬移等能力=玩家代码对抗世界规则（如引力），写对代码即可实现，不收任何税；唯有持续存在的复杂造物（城市/阵法/防御体系）想"发挥最大能力"时，玩家可自备服务器接入世界做实时解算加持——同构现实攻防（防御强的软件背后有大型服务器集群实时对抗恶意流量）。离线/无外接=本体基础功能照跑；在线+外接=体系满血。该"外接算力桥"是期五级大件（接入协议+世界侧握手），机制未实现，本条只立概念防漂移。
- 原因：土豆概念级纠正（原话要点）：飞行瞬移是代码功能"应该不需要额外消耗什么算力"，伪算力税"只为了增加玩家难度没有意义"；真算力出现在"留存城市/阵法并想发挥最大能力"需自备服务器加持——灵感同源自此前阵法研究（在线阵法连接玩家服务器）与现实软件攻防（大服务器实时解算的防御难被攻破）。助手曾将 fuel 误当生命值设计飞行代价，查 main.rs:210-212 fuel 每拍重置证实为保险丝，遂修。
- 否决：悬停/飞行收 fuel 税（伪约束，见定案①）；把 fuel 改造成生命资源（与生存点数双货币打架，违 #001-004 攻击语义铁律）；外接算力 MVP 化（接入协议是期五大件，概念先行防设计反复）。
- 落点：docs/decisions.md（本条）；方案 B 修订版=操控飞行无税（飞行/悬停强弱由物理与代码决定，被打断=坠落伤害照常，无人造代价）；期五候补议题"外接算力桥"。
