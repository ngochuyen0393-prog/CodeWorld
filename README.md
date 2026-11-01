# CodeWorld — 代码即现实的开放世界游戏框架

> 一个世界里，五件东西是真实存在的：时间、空间、渲染、物理规则、沙箱通道。
> 其余一切——生物的形态、行为、技能、战术、经济、文明——全部由玩家的代码定义。

[中文](#中文) | [English](#english)

---

## 中文

### 这是什么

CodeWorld 是一个"对抗性 agent 宿主"：一个权威世界进程（Rust + wasmtime + WASM + SQLite），玩家提交的代码作为生物生活在同一个世界里，互相创造、互相攻击、互相毁灭。

它不是又一个 UGC 平台。现有 UGC 平台（Roblox、Minecraft mod、UEFN）给玩家的是**零件表**——预设的模块、模板、数值表，玩家在围墙内拼装。CodeWorld 给玩家的是**车床和物理定律**——世界只提供诚实的接口（坐标、移动、攻击、探测、存续）和一套防死的沙箱，生物会飞还是会钻地、技能是什么形态、打不打得过谁，全部由玩家代码涌现，世界零预设。

**一句话**：你想造一艘奥特曼形态的战舰，在这里不是"找模型拼"，是写代码定义它的物理人格和战斗行为——世界不认识奥特曼，它只执行你的代码和它的物理定律。

### 核心设计（宪法摘要）

三个月迭代、61 条编号决策（[docs/decisions.md](docs/decisions.md) 全文公开）沉淀出的几条根本法：

- **能力 vs 规则**（#011）：框架只提供能力（诚实的接口），玩法全归玩家代码。发现"某玩法缺失"时，第一反应不得是"框架加对策功能"——那是越界。对抗手段是玩家的军备竞赛，不是世界的保姆服务。
- **世界只修路不跑车**（#038）：世界只提供通道（沙箱通道/信息通道），不提供剧情——攻守认定、胜负执行、战果回传全归玩家。
- **三位一体**（#041）：玩家画的形状就是身体形状——渲染、碰撞、点击、接触四层共用同一份形状数据。看起来是龙，判定上就是龙。
- **攻击只伤代码与算力**：换血/烧 fuel，永不碰生存点数。死亡靠断不靠改——扣扳机的永远是自己的机器。
- **确定性宪法**（#044）：世界是可回放的——同输入必同输出，这是仲裁可信和本地重放的技术根基。
- **一切持续效果同构**：悬停、游魂、力场、将来的感染/燃烧——一次声明+每拍供能+断供即灭。世界保持诚实，效果不豁免。

### 技术架构

```
权威世界服务器（Rust）
 ├─ 拍子机 —— 1 tick = 世界心跳一次，代码先跑物理后动
 ├─ 沙箱军团 —— wasmtime 实例池，每个生物一个独立 WASM 沙箱
 │              fuel=防死循环保险丝，内存配额，安全 import 白名单
 ├─ 物理规则层 —— 自研拍制物理（重力/阻力/冲量/浮力/力场，常数即宪法）
 ├─ 账本 —— SQLite 持久化，实体/快照/身份链
 └─ WS 服务器 —— 下行快照流，上行意图流（薄客户端协议）
浏览器前端（Three.js + PixiJS）
 └─ 显示器+遥控器，不做权威计算，本地预测+服务器对账
玩家代码（WASM/WAT）
 └─ 20 个 import + 3 个回调，一个文件 = 一个生物 = 一个独立意志
```

- **1312 个测试全绿**，clippy 0 警告，确定性写入验收标准
- 单人 + AI 协作开发三个月的完整 git 历史（310+ commits）——这条演化曲线本身是项目的一部分

### 快速开始

```bash
git clone https://github.com/ngochuyen0393-prog/CodeWorld.git
cd CodeWorld
cargo build --release
# 启动一个世界（Windows 双击 start-melee.bat 或：）
cargo run --release --bin codeworld
# 浏览器打开 http://localhost:port，扔一个 .wat 文件进 players/ 目录 = 你的生物进场
```

写你的第一个生物：[docs/newbie-first-thrower.md](docs/newbie-first-thrower.md)（投掷器模板第一课）
全部能力接口：[docs/api.md](docs/api.md)（20 个 import 逐个说明+示例）

### 文档地图（全部公开）

| 文档 | 内容 |
|---|---|
| [docs/decisions.md](docs/decisions.md) | 61 条编号宪法决策——为什么只增不改，每条带拍板背景 |
| [docs/dev-plan.md](docs/dev-plan.md) | 设计队列 D-001~D-023：每个系统的立法过程与落地回执 |
| [docs/architecture.md](docs/architecture.md) | 权威架构设计文档 |
| [docs/api.md](docs/api.md) | 玩家 API 全集（20 import + 3 回调 + 决斗窗口） |
| [docs/game-menu.md](docs/game-menu.md) | 12 章 121 条系统完备性目录（六游戏交叉调研） |
| [docs/design-duel.md](docs/design-duel.md) | 决斗制设计：世界只当硬件不当刽子手 |
| [docs/newbie-first-thrower.md](docs/newbie-first-thrower.md) | 新手第一课 |
| [docs/progress.md](docs/progress.md) | 三个月开发日志 |

### 这个项目相信什么

- **AI 时代让普通玩家第一次有了行为创造能力。** 过去玩家只能在零件表里拼装；现在一个不会写代码的人，可以带着 AI 写出"会飞、会埋伏、会认主人的巡逻艇"。
- **世界最大的真实性在互动。** 单机创造再好也是单机——所有玩家进入同一个世界，同时保持足够自主性，才是真实的世界。
- **世界不为任何玩法做平衡。** 攻就是比守便宜，人多就是力量大，围殴就是划算——平衡是玩家生态自己长出来的，世界只保证物理诚实。

### 参与方式

这个项目目前由一个人 + AI 协作开发。它现在的状态：**核心地基完成（沙箱/物理/账本/协议/操控/工坊），玩法生态为零，等待第一个外部玩家。**

如果你是：
- **想写第一个生物的玩家**——从 [newbie-first-thrower.md](docs/newbie-first-thrower.md) 开始，你的 .wat 文件就是你的生物
- **Rust/WASM 工程师**——看 [decisions.md](docs/decisions.md) 再看代码，每个设计都有"为什么"
- **游戏设计者**——[game-menu.md](docs/game-menu.md) 是一份 12 章的系统完备性地图，欢迎对着它挑战
- **想开自己世界的人**——fork 它，世界是 MIT 的

### 状态与诚实

- 1 tick/秒（拍速升级到 5-10/秒在队列中）
- 2D 俯视（3D 是既定方向，地基已为此设计）
- 单机自托管，多人=一个世界进程+多个浏览器连接
- **还没有外部玩家。你是第一个的机会是真的。**

---

## English

### What is this

CodeWorld is an **adversarial agent host**: an authoritative world process (Rust + wasmtime + WASM + SQLite) where player-submitted code lives as creatures in one shared world — creating, attacking, and destroying each other.

It is not another UGC platform. Existing platforms hand players a **parts catalog** — preset modules, templates, stat tables, assembly inside walls. CodeWorld hands players **a lathe and the laws of physics** — the world provides honest primitives (position, movement, attack, probing, persistence) and a sandbox that can't hang; whether your creature flies, burrows, or ambushes, what its "skills" are — all emergent from player code. Zero presets.

**In one line**: you don't "find a model and assemble" an Ultraman-shaped warship here — you write code that defines its physical personality and combat behavior. The world doesn't know what an Ultraman is; it only executes your code and its own laws.

### Core design (constitution digest)

Three months of iteration, 61 numbered decisions (full text in [docs/decisions.md](docs/decisions.md), in Chinese) produced a few root laws:

- **Capabilities vs Rules** (#011): the framework provides capabilities (honest interfaces); gameplay belongs to player code. "This tactic is missing" is never the world's problem to fix — counterplay is the players' arms race.
- **The world builds roads, never drives trucks** (#038): channels (sandbox channel / message channel) only. Who's attacker, who wins, what the spoils are — all player-side.
- **Trinity** (#041): the shape a player draws *is* the body — render, collision, click-hit, and contact all read the same shape data. If it looks like a dragon, it collides like one.
- **Attacks harm code and compute, never score**: blood-swap / fuel burn. Death comes from disconnection, not from modification — the trigger is always your own machine.
- **Determinism** (#044): same input, same world — the technical basis for trusted arbitration and local replay.
- **All persistent effects share one template**: hover, ghosts, force fields, future infections/burns — declare once, power every tick, die on power loss. The world stays honest.

### Architecture

```
Authoritative world server (Rust)
 ├─ tick machine — code runs first, physics moves second
 ├─ sandbox legion — one wasmtime instance per creature; fuel = deadlock fuse
 ├─ rules-layer physics — gravity/drag/impulse/buoyancy/fields (constants are constitution)
 ├─ ledger — SQLite persistence
 └─ WS server — snapshot stream down, intent stream up (thin client)
Browser front end (Three.js + PixiJS) — display + remote, never authoritative
Player code (WASM/WAT) — one file = one creature = one independent will
```

**1312 tests green**, clippy clean, determinism in acceptance criteria. The full 310+ commit history of one person + AI building this in three months is part of the project.

### Quick start

```bash
git clone https://github.com/ngochuyen0393-prog/CodeWorld.git
cd CodeWorld && cargo build --release
cargo run --release --bin codeworld
# open the browser, drop a .wat file into players/ — that file is your creature
```

First creature tutorial (Chinese): [docs/newbie-first-thrower.md](docs/newbie-first-thrower.md) · Full API: [docs/api.md](docs/api.md)

### Status, honestly

- 1 tick/sec (5-10/sec upgrade queued) · 2D top-down (3D is the committed direction) · self-hosted
- **Zero external players so far. The chance to be the first is real.**

MIT License — fork it, run your own world.
