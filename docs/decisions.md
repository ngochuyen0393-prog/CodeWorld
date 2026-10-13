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
