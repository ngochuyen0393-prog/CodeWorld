# TASK（期五第二刀·小刀）：登出弹窗交互修复——决策即关门 + 镜头归属

## ⚠️ 纯前端单文件改动（bin/codeworld/static/index.html）；Rust 侧零改动零回归；前端逻辑跑 node 纯函数验证

## 问题（土豆实测截图实锤）

选完处置选项（如"留下·等 AI"）后：世界已更新（顶部"已登出"横幅已出现、角色状态已变），但**三选弹窗仍堵在屏幕中央不关闭**，需要另想办法关掉，影响使用。

## 业界依据（已调研）

Unity/游戏确认弹窗与 modal UX 通用标准：**弹窗是一次决策的场所——点任何一个选项=决策完成=弹窗立刻关门**（0.25s 淡出，Unity Color Tint Fade Duration 标准），"弹窗不该需要用户再点一次关闭"；决策结果交给非阻塞的状态展示（我们已有顶部横幅，正确）；焦点还给游戏画面。防误触设计：取消按钮绿色（继续玩）、确认类红色渐进（Unity VR 教程标准）——我们已有红登出/绿重进，一致。

## 需求（全部在 index.html）

### 1. 决策即关门
- 点三个选项任一卡片：卡片按下高亮一拍（视觉回执，~120ms）→ 发 logout 消息 → 弹窗整体 0.25s 透明淡出 → display:none → 焦点回 canvas
- 点"取消（回到世界）"：同款 0.25s 淡出（不发任何消息）
- Esc 键：等同取消（仅当弹窗开着）
- 淡出期间禁止重复点击（防双发 logout）

### 2. 登出后的镜头归属
- 现状 bug：选"留下·等 AI"后镜头仍跟随 D（截图实锤：准星+绿 D）——但语义上"这是你的角色在待命"
- 修：登出后（已登出状态）镜头跟随目标不变（仍跟自己的 P——它还在世界里），P 不存在（take 带走/被杀）才回落全景。现状 meRendered 逻辑大概率已支持（P 实体还在快照里），实测确认即可；若登出后 controlled 实体查找失效导致回落全景/错跟 D，修 findControlled 逻辑（登出态仍按 me.txt 声明的名字找实体）
- 镜头跟随的实体死了/带走了：现状"自动回落全景"保留（正确）

### 3. 状态自愈
- 已登出状态下再按 L / 点登出按钮：无操作（已登出不能再登出，按钮禁用态或直接忽略）
- "重新进入世界"点击后：横幅消失、按钮恢复可用、操控认领（现状已有，验证不断裂）

### 4. 验证
- PURE 区若有相关纯函数（弹窗状态机）→ node 验证；无则不硬造
- 主要靠土豆人工验收（弹窗是视觉交互，无 Rust 逻辑）
- Rust 侧零改动：cargo test 不需要跑全量（无 .rs 文件改动——git status 确认后跳过；如有意外 .rs 改动必须全量跑）

## 验收标准（土豆浏览器）

```
a. 点登出 → 弹窗出 → 点「留下·自主」→ 卡片闪一下 → 弹窗 0.25s 消失
   → 横幅在、P 自己走、镜头跟着 P ——全程无"还要关弹窗"的尾巴
b. 点登出 → 点「取消」→ 弹窗淡出 → 回到操控（什么都没发生）
c. 点登出 → 按 Esc → 同 b
d. 登出弹窗开着时连点选项卡片两下 → 只发一次 logout（battle.log 一条）
e. 已登出状态按 L → 无反应（不会二次弹窗）
f. 「带走」登出后（P 消失）→ 镜头回落全景（不跟丢）
g. 「重新进入世界」→ 横幅消失、操控恢复、可再次正常登出
```

## 已知信息（不需要查）

- 弹窗 DOM/三卡片/取消按钮：上一刀（6a92f2e）加的，搜 logout/登出 关键字定位
- 横幅+重新进入世界按钮：同刀，保留不动
- CSS 全部内联在 index.html 的 style 块；transition/opacity 淡出用 CSS transition 即可（零依赖）
- 焦点回 canvas：canvas 本身不抢焦点（点击处理在 window 层），弹窗 display:none 后鼠标事件自然回画布——验证即可，可能不需要显式代码
- 镜头跟随逻辑：meRendered/camMode/cam 一组变量（第四刀 C 项），findControlled(lastState.ents) 按 controlled 标记找——登出后快照里 P 的 controlled 字段是否还标着？**查上一刀快照序列化：登出后 disposition=auto/ai-await 时 controlled 是否仍 true**——若 false 则镜头丢目标回落全景，需按"已登出仍跟随"修（用 name 匹配或 disposition 辅助）
- battle.log 的 logout 处置日志格式：上一刀 [dispose] 系列

## 硬 Gate

无 Rust 改动则跳过 cargo；改动任何 .rs 文件则三连全跑：
```bash
cd D:/projects/CodeWorld && cargo test --workspace
cd D:/projects/CodeWorld && cargo clippy --workspace --tests -- -D warnings
cd D:/projects/CodeWorld && cargo check --workspace
```

## 交付物

1. 中文 commit（fix: 期五第二刀——登出弹窗决策即关门+镜头归属）
2. C:/Users/Administrator/AppData/Local/Temp/cc-report.md（简版两节：改了什么/土豆验收步骤）

不要弹选项；信息缺失超3次试错即停写报告。
