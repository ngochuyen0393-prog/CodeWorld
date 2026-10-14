# 项目当前状态

> 收工前由 Claude 更新「现在」一节；完成的移入「已完成」。

## 现在（2026-10-14 实体占位制收工更新）
- 在做：无（#021 实体占位制落地：一格一物——移动被挡+接触记双方+spawn 出生预检+推挤退役，pioneer.wat 塔 +5+立塔拍不移动修复连体事故，495 测试）
- 卡在：无
- 下一步：① wasmtime 25.0.3 Windows out-of-fuel trap 进程 abort 仍是生产化阻断项（.cc-bugs #5）；② 已知 MVP 缺口：probe 残影实体仍会答 on_probe（桥不感知残影，#016）；③ 接触开通道（#021 已记双方接触对日志，通道未开）；④ 门二后续：destroy 的"攻杀式销毁"通路、产权链转让/继承、造物重生（快照恢复不带回沙箱）；⑤ 半径制将来路线：体型开放时四件套（上下限/面积付费/总预算/半径判定）同天上线（#021）；⑥ Phase 2 议题同前（排刀上限、并行执行、multi-value 列表式 get_visible_entities）

## 已完成
- 实体占位制 #021（2026-10-14）：core 占位判定 cell_occupied_by（floor 格键/排除自己/只算存活/多占据返最低 id）+ physics step 占位重力（落被占格悬停上一格，id 升序确定序）+ 移动占位裁决（SetPos 抽出按 EntityId 升序先到先得，被挡留原位+记接触对）+ 接触记双方（ContactPair 两类来源 same_cell/move_blocked 汇入 handle_collisions 逐对 info 日志）+ spawn 出生预检（被占失败可重试，含创造者自己的格）+ 推挤退役（resolve_collision 标 deprecated 留档，同格只 warn 不推挤）+ pioneer.wat 修复（塔 +5/立塔拍 return 不移动，冒烟实测 P 东移至塔前被挡、T 全程站岗）；TDD 红绿全程，21 新测试，四生物剧本时间线回归不变（tick4 H 死/tick12 A 死照旧），495 测试（2026-10-14）
- 门二开放 #020（2026-10-14，commit fd9ff89）：env.spawn/env.destroy 给玩家代码 + 产权链 owner + examples/sentinel.wat 哨塔示例，474 测试；世界里第一个玩家造物已诞生——冒烟测试进程内可复现
- Phase 1 MVP 全部 8 crate（333 测试基线，见项目 CLAUDE.md 开发顺序）
- MVP 核心缺口 P0/P1（2026-07-23/24）
- 击杀掠夺 Phase 2 攻击流程（2026-07-24，commit 99673cc）
- 攻击流程修复与两阶段结算：P0 僵尸/P1 三条/两阶段模型/铁律入档，审查循环 2 轮收敛（2026-10-13，commit 6adac48 + 5e8d0d4 + dfd311d，348 测试）
- 最简可玩版：get_pos/scan/move_by 三 import + 感知快照 + 移动队列 + 两只真性格生物（A 冲锋 / D 盾+反制）+ 两种结局参数路径锁定 + start.bat 双击可玩，365 测试（2026-10-13）
- 可玩性三连：① 战况字幕（render caption 模块写终端第 1 行，盾量经 env.log marker 玩家自愿上报解析，距离/存活数取 world 快照）；② 翻盘实验（CODEWORLD_D_SHIELD 配置化 + start-reversal.bat 薄盾纯对决剧本）；③ 第三只生物 H 黄"H"投机者（>3 格观望游走 / ≤3 格参战 / 奇偶 50% 拦截，CODEWORLD_H_ENABLED 开关），三剧本冒烟全过（默认 D 赢 / 翻盘 A 赢 / 混战 H 出刀），402 测试（2026-10-13）
- 三层感知 Phase A：① 眼睛层 env.visible_count/visible_at（host 阶段一前 viewport 快照，Uuid 升序排除自己）；② 探测层 env.probe（坐标寻址 + ProbeBridge raw 指针桥同步跨沙箱 + on_probe 独立 fuel 预算 CODEWORLD_PROBE_FUEL + 24 字节被动体徵尾块）；③ 伪装者 S 紫"S"（每 4 tick 侦察 marker=10 + probe 最近者，on_probe 回 "LIE9" 假串，脆皮无防守，CODEWORLD_S_ENABLED 开关）+ D 反制前 probe 看一眼（撒谎串实战可验证），四剧本冒烟全过，425 测试（2026-10-14）
- 期一·门一进场（world-foundation 路线第 1 步）：players/ 目录进场（.wat=玩家，名字/字形/色轮 green 起/出生点 30-45 循环，CODEWORLD_PLAYERS_DIR 可覆盖，编译失败黑名单+删文件重放可重试）+ 在线锚（每 tick 轮询，新文件进场/删除下线）+ 战斗残影期 #013（参战记录 30 拍窗，残影期 max(5,30-距参战拍数)，残影完全惰性可被杀掠夺）+ docs/api.md 玩家接口宪法 v1 + players/example.wat 模板，审查 1 轮修 P1（落空刀也记参战），冒烟全链路过，452 测试（2026-10-14）
- scan 视野化（感知回归设计稿 4.2，#019）：find_nearest_enemy 加 viewport（BoundingBox）过滤（与 fill_visible_snapshots 同 tick 同一实例，同源口径），视野内无目标返 0；TDD 红绿 5 新测试（视口内/外过滤 2 + scan 端到端 1 + 冒烟 2：默认视口四生物+E 时间线 tick4 H 死/tick12 A 死不变、缩小视口 [-10,10] 屏外 E 全程无伤且 A 锁不到屏外 D 盾不掉），A/D/H WAT 原有 scan=0 分支免改，api.md/decisions.md 同步，457 测试（2026-10-14）
