# TASK（轻装挡）：悬浮间谍——S 抵消重力悬停高空

## 任务类型：轻装（小改动+单审+便宜模型可跑）

## ⚠️ 必须使用 test-driven-development（红→绿）

## 项目上下文

项目：D:/projects/CodeWorld（Rust workspace，基线 425 测试全绿，git f77b42f）
问题根源（已人赃俱获，不要重新排查）：
- S（spy）出生 (25,12) 空中，物理重力 gravity=-5.0/拍（crates/physics/src/default.rs:239），**第一拍即坠落至地面 (25,0)**，之后一直站地面
- D 站桩反制最近敌人，scan 报 (25,0) 属实 → 刀命中 → S 被远程点杀
- decisions.md #009 写的"右侧高台"只是叙事，物理上不存在高台
- 结论：S"高台放哨够不着"的人设被重力废掉

## 需求（本任务只做 A 方案：悬浮，不建地形系统）

改 SPY_WAT（bin/codeworld/src/main.rs:1242 起）的 on_tick：**每拍 move_by(0, 悬浮量) 抵消重力**，让 S 稳定悬停在空中（目标高度 ~12，允许围绕它小幅波动但永不落地）。

要点：
- 重力每拍 -5.0。S 的 on_tick 每 4 拍才进侦察逻辑（rem_u 4 提前 return），**悬浮动作必须在 4 倍数拍之外的每一拍也执行**——把悬浮挪到计数自增之后、侦察 if 之前（每拍先悬浮再判断是否侦察）
- 悬浮策略自定（简单版：y < 12 则 move_by(0, +5.0) 抵消当拍重力；或直接无条件 move_by(0, +5.0)——物理先 apply 重力再 apply pending 的话顺序要想清楚，读 main.rs tick 阶段二 a 步与物理步的先后，保证净效果≈悬停）
- move_by 的拒绝条件：目标 y<0 拒绝、非有限拒绝（已实现，勿动）
- 不改 physics crate、不加新 import、不建地形——超出范围即违规
- SPY_WAT 现有侦察/probe/on_probe 逻辑保持不变，只是把"每4拍侦察"改成"每拍悬浮+每4拍侦察"
- 注释同步更新（S 的人设描述加"悬浮"）

## 验收标准

1. 新测试：跑 ≥6 拍（含 4 倍数拍），S 的 y 坐标全程 > 5（从未接近地面）；悬浮不受侦察节奏影响（第 1/2/3 拍也在悬浮）。测试用 host 侧断言（tick 后读 world 里 S 实体坐标），参考现有 spawn/battle 测试模式
2. 既有 425 测试零回归（特别注意剧本 4 撒谎测试——S 悬浮后坐标变了，若测试硬编码了 (25,0) 需按新行为修正断言并注明）
3. 冒烟：实跑一局 ≥30 拍，日志证明 S 全程 y>5 且 D 的刀不再命中 S（攻击落空或打不到）；S 若仍被杀只能是"有人飞上去贴脸"（本剧本没人会飞，预期 S 活到剧终）
4. clippy/check 三连过

## 已知信息（不需要查）

- gravity=-5.0/拍，物理步在 tick 阶段二（apply_pending_changes 之后）执行
- move_by 拒绝条件：dx/dy 非有限、目标 y<0、目标点非有限
- S 无防守无攻击，唯一自卫=够不着
- tick 顺序：阶段一玩家代码（含 move_by 排队）→ 阶段二 a 应用 pending → 物理步（重力）→ …
- SPY_WAT 观测导出 get_probe_ret 已有，别动
- cargo 用法：PATH 无 cargo 时 node C:/Users/Administrator/AppData/Local/Temp/cc-cargo.mjs 转发（文件不存在则重建）

## 硬 Gate

```bash
cd D:/projects/CodeWorld && cargo test --workspace
cd D:/projects/CodeWorld && cargo clippy --workspace --tests -- -D warnings
cd D:/projects/CodeWorld && cargo check --workspace
```

## 审查（轻装挡：单审一轮）

会话内 Agent 子代理 ×1，聚焦：悬浮逻辑正确性（净高度变化分析）+ 是否越界改动 physics crate + 425 零回归确认。无 P0/P1 即收工。

## 交付物

1. 中文 commit（feat: 悬浮间谍——S 抵消重力悬停）
2. C:/Users/Administrator/AppData/Local/Temp/cc-report.md（简版：改动/测试结果/冒烟摘录三节即可，轻装挡不强制四节全）

不要弹选项让用户选；信息缺失超 3 次试错即停写报告。
