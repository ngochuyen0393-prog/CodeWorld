# TASK（期四第二刀，中等任务）：Canvas 观战画面 + 快照插值

## ⚠️ 前端为手写原生 JS（项目零前端工具链宪法），但逻辑测试用 Rust 侧覆盖不了——本刀验收以"土豆浏览器人工验收"为准，JS 质量靠代码审查

## 宪法依据

decisions.md #026（期四定案：薄客户端/快照推送/插值路线/原生Canvas零框架）。快照契约：docs/net-protocol.md v1（字段表已定，前端照实现）。

## 业界对标

snapshot interpolation（Valve Source/通用 netcode）：客户端渲染"在过去"——保留最近两帧快照，渲染时间落在两帧之间平滑插值。1秒/拍的世界呈现近似实时画面。

## 项目上下文

项目：D:/projects/CodeWorld（Rust 侧不动！本刀 99% 是前端文件），基线 554 测试全绿，git 640ff17
已有：
- WS 服务：axum，每拍推全量 JSON 快照（tick/caption/viewport/entities[id,glyph,color,x,y,kind,name,controlled,owner]）
- 验收页：static/index.html（本刀重做它）
- net-protocol.md v1 契约
- 终端 ANSI 画面与 WS 并行（都活着，浏览器不是替代是升级）

## 需求

### A. Canvas 战场画面（重做 index.html）
1. 布局：顶部字幕条（caption 原文，等宽字体）+ 主画布（Canvas，自适应窗口）+ 底部状态栏（tick/实体数/连接状态/断线提示）
2. 画布内容：
   - 世界坐标→画布像素：viewport（x0,y0,w,h）映射整个画布（铺满）
   - 背景：深色网格（每格一条暗线，格子感）
   - 地面线：y=0 处一条稍亮的横线（世界地面）
   - 实体：每格中心画 glyph 字符（Canvas fillText），颜色用快照 color；kind=creation 可加方形底衬区分；controlled 实体加白色描边框或光圈（高亮操控目标）
   - 悬浮提示（可选加分项）：鼠标悬停实体格显示 name/kind（title 层 div，简单实现）
3. 性能：每帧重画全画布（60fps，实体≤64，无压力）；requestAnimationFrame 驱动

### B. 快照插值（本刀灵魂）
1. 维护快照缓冲：保留最近 N 帧（≥2），每帧带本地接收时间戳
2. 渲染时间 = 当前时间 - 插值延迟（默认 = 1.5×平均帧间隔，即"渲染在过去"）
3. 实体位置 = 在两帧快照间线性插值（按渲染时间落在哪两帧之间）
4. 新实体出现：从出生帧位置直接显示（无插值）；实体消失：从缓冲剔除（死亡即消失，可接受）
5. tick 间隔自适应：从实际帧到达间隔滚动平均计算（CODEWORLD_TICK_MS 可变时自动跟随）
6. 断线处理：WS 断开→底部状态栏红字"已断线，等待重连"→自动重连（指数退避）→重连后清空缓冲重新开始

### C. 操控预备（第三刀的地基，本刀只做显示）
- controlled 实体高亮（A2 已含）
- 鼠标位置显示：状态栏显示鼠标悬停的世界坐标（为第三刀点击做直观校验）

### D. Rust 侧微调（仅必要时）
- 若快照缺字段（如 name 缺失影响悬浮提示）：补齐 + net-protocol.md 更新 v1.1（报告写清 diff）
- 其他一律不动 Rust

### E. 文档
- net-protocol.md：如有 v1.1 变更记录
- progress.md 更新
- index.html 顶部注释：架构说明（插值原理一段话）+ 手写零依赖声明

## 验收标准

1. 土豆浏览器验收（报告写明步骤）：
   a. start.bat（带 WS）→ localhost:8080 → 看到战场：网格+地面线+彩色实体（A红/D青/S紫/H黄/塔青底/P绿+白圈）
   b. 实体平滑滑动（不是每秒跳格）——插值生效的直观判据
   c. 字幕条实时更新（tick/盾量/alive/ctrl）
   d. 拔掉 WS（Ctrl+C 世界）→ 断线提示 → 重启世界 → 自动重连恢复画面
   e. 悬停实体看到 name/kind
2. 554 基线零回归（Rust 若有微调，测试同步）
3. clippy/check 三连过（Rust 侧）
4. JS 审查要点（单审一轮）：插值数学（帧间定位/边界：帧不足/时间溢出）、缓冲溢出、断线重连状态机、坐标映射含负数域、fillText 性能（每帧≤70次调用）

## 已知信息（不需要查）

- 快照每拍全量（1秒/拍默认），JSON 已含 viewport 域（x0=-50,y0=-25,w=100,h=50）
- 颜色字段值是 CSS 色名（red/cyan/purple/yellow/green/magenta…），fillStyle 直接用
- glyph 是单字符（A/D/H/S/P/T/E…），fillText 居中（textAlign center + textBaseline middle）
- WS 地址：同源 /ws（页面和 WS 同端口 8080）
- 世界 y 向上为正（y=0 地面，S 在 y=12）——画布 y 要翻转（世界y=0 在画布下方）
- 实体 x/y 是 f64 连续坐标（A 的斜向移动非整数）——插值后更平滑，直接用
- 断线重连的页面测试：可以临时 kill 世界进程再启动（土豆验收步骤里写）
- 浏览器兼容：现代 Chrome/Edge 即可（WebSocket/Canvas/rAF 都是十年级标准）

## 硬 Gate（Rust 侧；JS 无 cargo gate，以审查+人工验收代）

```bash
cd D:/projects/CodeWorld && cargo test --workspace
cd D:/projects/CodeWorld && cargo clippy --workspace --tests -- -D warnings
cd D:/projects/CodeWorld && cargo check --workspace
```

## 审查（单审一轮）

聚焦：插值边界（缓冲<2帧/渲染时间落在缓冲外/首帧）；断线状态机不死循环；坐标翻转正确（S 在画面上方）；受控高亮；零依赖声明属实（无 CDN/script src）。

## 交付物

1. 中文 commit（feat: 期四第二刀——Canvas战场+快照插值）
2. C:/Users/Administrator/AppData/Local/Temp/cc-report.md（简版三节+土豆验收步骤清单）

不要弹选项；信息缺失超3次试错即停写报告。
