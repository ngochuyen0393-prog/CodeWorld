# 项目当前状态

> 收工前由 Claude 更新「现在」一节；完成的移入「已完成」。

## 现在（2026-10-14 悬浮间谍收工更新）
- 在做：无（悬浮间谍轻装挡已收工：S 每拍 move_by(+9.8) 抵消重力悬停 y≈12，反制刀坐标过期落空，426 测试）
- 卡在：无
- 下一步：① wasmtime 25.0.3 Windows out-of-fuel trap 进程 abort 仍是生产化阻断项（.cc-bugs #5；probe 预算机制已在其约束下落地，trap-free 待升级）；② 三层感知 Phase B（如 call 通道/伪装深化：体徵比对辅助、response 完整 Bytes 传递）；③ Phase 2 议题同前（同坐标多实体、排刀上限、并行执行、multi-value 列表式 get_visible_entities）

## 已完成
- Phase 1 MVP 全部 8 crate（333 测试基线，见项目 CLAUDE.md 开发顺序）
- MVP 核心缺口 P0/P1（2026-07-23/24）
- 击杀掠夺 Phase 2 攻击流程（2026-07-24，commit 99673cc）
- 攻击流程修复与两阶段结算：P0 僵尸/P1 三条/两阶段模型/铁律入档，审查循环 2 轮收敛（2026-10-13，commit 6adac48 + 5e8d0d4 + dfd311d，348 测试）
- 最简可玩版：get_pos/scan/move_by 三 import + 感知快照 + 移动队列 + 两只真性格生物（A 冲锋 / D 盾+反制）+ 两种结局参数路径锁定 + start.bat 双击可玩，365 测试（2026-10-13）
- 可玩性三连：① 战况字幕（render caption 模块写终端第 1 行，盾量经 env.log marker 玩家自愿上报解析，距离/存活数取 world 快照）；② 翻盘实验（CODEWORLD_D_SHIELD 配置化 + start-reversal.bat 薄盾纯对决剧本）；③ 第三只生物 H 黄"H"投机者（>3 格观望游走 / ≤3 格参战 / 奇偶 50% 拦截，CODEWORLD_H_ENABLED 开关），三剧本冒烟全过（默认 D 赢 / 翻盘 A 赢 / 混战 H 出刀），402 测试（2026-10-13）
- 三层感知 Phase A：① 眼睛层 env.visible_count/visible_at（host 阶段一前 viewport 快照，Uuid 升序排除自己）；② 探测层 env.probe（坐标寻址 + ProbeBridge raw 指针桥同步跨沙箱 + on_probe 独立 fuel 预算 CODEWORLD_PROBE_FUEL + 24 字节被动体徵尾块）；③ 伪装者 S 紫"S"（每 4 tick 侦察 marker=10 + probe 最近者，on_probe 回 "LIE9" 假串，脆皮无防守，CODEWORLD_S_ENABLED 开关）+ D 反制前 probe 看一眼（撒谎串实战可验证），四剧本冒烟全过，425 测试（2026-10-14）
