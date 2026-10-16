# TASK（期四第八刀·小刀）：S 获得防御代码——on_attack 拦截盾（学 D 的盾机制，间谍的第二次进化）

## ⚠️ TDD：S 的行为变更需要测试锚定（红→绿）；改动集中在一个 WAT 重写 + 若干测试

## 宪法依据

#029（能力=代码，无税：防御是 S 代码的能力）；#001-004（攻击只伤代码与算力，拦截=on_attack 语义，D 已有先例）；#031（点命中制：站稳=可被打中，防御代码是活下来的正路）。

## 项目上下文

项目：D:/projects/CodeWorld（基线 639 测试全绿，git cec139b）
现状：S（spy）在第七刀迁移 hover() 后站稳 (25,12)，但脆皮无防守——土豆实测 tick=1 被 P 一刀带走（操控空格攻击最近敌人，P 距 S 比距 D 近）。D 的防御先例：盾 10 格（每拦一刀 -1，每 4 tick 反制），WAT 侧 on_attack 拦截实现已在 defender.wat 内嵌字符串（main.rs 生物登场处）。
本刀：给 S 写防御——但 S 的防御应该有间谍人设特色，不是 D 的复制品。

## 需求

### S 的防御设计（人设：伪装者/间谍——"你砍中的可能不是我"）

**盾式拦截（简单可靠，本刀主体）**：
- S 的 on_attack：拦截攻击，护盾值 3（远低于 D 的 10——间谍是轻装）
- 每拦截一刀盾 -1；盾耗尽后 on_attack 失败（不再拦截），后续攻击照常结算——S 回到脆皮
- 盾不回复（一次性护盾，与 D 一致）

**间谍特色（一行灵魂，不复杂化）**：
- on_attack 拦截成功时，S 用 log() 输出一行伪装台词（如 "S: 那只是我的替身"）——间谍中弹不承认人设的一点点味道；log 内容随意但要有（battle.log 可见）
- 可选加分项（如果 WAT 实现简单）：S 每 16 拍自动挪窝一次（向随机方向 move 3-5 格再悬停）——"间谍不停留"的活感。**如果挪窝实现复杂（WAT 无随机数，需要用 tick 计数伪随机），跳过此项不算失败**，报告说明

### 实现要点
- S 的 WAT 在 main.rs 生物登场处内嵌字符串（搜 spy/伪装者 关键字找到现有 WAT）——重写加 on_attack export
- on_attack 的 WAT 写法参照 defender.wat（D）的拦截模式：读攻击 payload → 拦截决策 → 返回拦截结果
- 拦截语义走现有 deliver_attacks 通道（intercepted=true 时攻击不结算）——零机制改动，纯 WAT 层
- 盾计数需要状态：WAT 全局变量（mutable global）存盾值——参照 D 的实现（D 也是 WAT 全局变量存盾）
- 盾 3 的参数放显眼位置便于调（WAT 常量或注释）

### 测试
- Rust 测试：S 拦截前 3 刀（intercepted=true）；第 4 刀拦截失败（intercepted=false，进入正常结算）；S 的其他行为回归（hover 照常/probe 照旧/LIE9 假串照旧）
- 现有 spy 相关测试若断言"无防守"需更新（列出）

### 文档
- progress.md 更新；api.md 不动（hover() 已在）；decisions.md 不新增（#001-004 范围内的生物配置变更）

## 验收标准

1. TDD 红绿；639 基线零回归；clippy/check 三连过
2. CC 起服 headless 冒烟（若沙箱跑不了 exe 就用进程内测试）：S 被攻击 3 次全拦截（battle.log intercepted=true ×3 + S 的伪装台词 log）+ 第 4 次被击杀
3. 土豆验收：start.bat → 空格点 S 三次（或点它出刀三次）→ 看到 3 次拦截（battle.log "S: 替身"台词）→ 第 4 刀 S 卒
4. 审查：on_attack WAT 与 D 的拦截模式一致（无自创语义）；盾全局变量的初始/递减/耗尽路径；跳过挪窝项时报告说明

## 已知信息（不需要查）

- D 的盾 WAT 在 main.rs 内嵌（搜 defender），拦截模式照抄
- deliver_attacks 的 intercepted 语义已存在（D 每天在用），本刀零机制改动
- S 当前 WAT：hover() 每拍 + visible/probe 每 4 拍 + LIE9 假串 on_probe；重写时全保留
- S 盾 3：土豆需求"学 D 的盾但 S 有特色"，数值 3 是我的建议（轻装间谍），可在验收后调
- WAT 无随机数：挪窝用 tick 取模伪随机即可（tick % 4 选方向），实现不超过 15 行就做，超了跳过

## 硬 Gate

```bash
cd D:/projects/CodeWorld && cargo test --workspace
cd D:/projects/CodeWorld && cargo clippy --workspace --tests -- -D warnings
cd D:/projects/CodeWorld && cargo check --workspace
```

## 交付物

1. 中文 commit（feat: 期四第八刀——S防御盾+间谍人设）
2. C:/Users/Administrator/AppData/Local/Temp/cc-report.md（简版三节+土豆验收步骤）

不要弹选项；信息缺失超3次试错即停写报告。
