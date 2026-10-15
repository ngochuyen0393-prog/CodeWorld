# CodeWorld 网络协议 v1：快照推送（期四第一刀）

> 前端契约文档。第二刀（Canvas 渲染）依赖本文字段名与语义；改字段先改这里再改代码。
> 依据 decisions.md #026（薄客户端/快照推送）；世界是唯一权威，浏览器只是显示器。

## 1. 传输概要

| 项 | 值 |
|----|----|
| 协议 | WebSocket（升级自 HTTP） |
| 路径 | `/ws`（同源；静态页由同一端口 ServeDir 伺服） |
| 端口 | `CODEWORLD_WS_PORT`，默认 8080；**0 = 禁用 WS**（纯终端模式，零开销） |
| 绑定 | 127.0.0.1（仅本机浏览器可访问；局域网观战留后续） |
| 方向 | 服务器 → 客户端单向推送；客户端 → 服务器消息第一刀被忽略（只回 pong） |
| 频率 | 每拍一帧（`CODEWORLD_TICK_MS` 默认 1000ms 一拍） |
| 帧格式 | JSON 文本（`Message::Text`），全量快照（非增量） |
| 积压策略 | 慢客户端丢旧帧（后值覆盖语义），不排队压死浏览器 |

## 2. 顶层字段

```json
{
  "tick": 42,
  "caption": "tick=42 | A->D dist=2.4 | D shield=7 | alive=3 | ctrl:P",
  "viewport": {"x0": -50.0, "y0": -25.0, "w": 100.0, "h": 50.0},
  "entities": [ ... ]
}
```

| 字段 | 类型 | 说明 |
|------|------|------|
| `tick` | u64 | 世界当前拍数（WorldState::tick） |
| `caption` | string | 终端第 1 行战况字幕原文（含 `ctrl:` 操控段，#024 三态） |
| `viewport` | object | 世界坐标→画布坐标换算基准（见 §4） |
| `entities` | array | 全部存活实体的快照（含残影），按 `id` 字典序**稳定排列** |

## 3. 实体字段

| 字段 | 类型 | 出现条件 | 说明 |
|------|------|----------|------|
| `id` | string | 恒有 | 实体 UUID（连字符小写）；帧内排序键 |
| `glyph` | string | 恒有 | ASCII 字形（与终端渲染同源） |
| `color` | string | 恒有 | 颜色名（red/cyan/yellow/purple/green/magenta/blue/white） |
| `x`, `y` | f64 | 恒有 | 世界坐标（f64 原值，未取整；画布换算见 §4） |
| `kind` | string | 恒有 | `"npc"` / `"player"` / `"creation"`（见 §3.1） |
| `name` | string | 仅 player | 玩家名（= code_hash 身份承载，players/ 目录文件名） |
| `controlled` | bool | 仅 player | `true` = me 声明的操控目标（#024，每拍解析）；player 字段恒在（前端 `e.controlled` 直接可用） |
| `owner` | string | 仅 creation | 创造者 PlayerId 的 UUID 字符串（产权链 #015） |

npc 实体**没有** `name`/`controlled`/`owner` 键（`skip_serializing_if` 保持 JSON 干净）。

### 3.1 kind 判定（code_hash 纯映射）

| code_hash | kind |
|-----------|------|
| `attacker` / `defender` / `hunter` / `spy` | `npc`（开局四生物） |
| `creation` | `creation`（门二造物，带 owner） |
| players/ 目录在线玩家名 | `player` |
| 残影躯壳（#013，已下线未消散） | `player`（code_hash 仍是玩家名；`controlled` 恒 false） |
| 其他未知 | `npc`（兜底） |

## 4. viewport 换算基准

```json
{"x0": -50.0, "y0": -25.0, "w": 100.0, "h": 50.0}
```

- `x0`/`y0`：视口**左下角**世界坐标；`w`/`h`：世界坐标宽高
- 与终端渲染视口同源（`Viewport::world_x_range/y_range`），随 `CODEWORLD_VIEWPORT_*` 配置变化
- 画布换算（第二刀用）：`canvas_x = (entity.x - x0) / w * canvas_w`；
  注意世界 y 向上、屏幕 y 向下时需翻转：`canvas_y = canvas_h - (entity.y - y0) / h * canvas_h`
- 当前值域：x ∈ [-50, 50]，y ∈ [-25, 25]（默认配置）

## 5. 连接生命周期

| 事件 | 服务器行为 |
|------|-----------|
| 连接建立 | info 日志，订阅快照广播，下一拍起收帧 |
| 无浏览器连接 | 主循环跳过快照组装（零开销，不序列化不发） |
| 客户端 Ping | 回 Pong（载荷原样） |
| 客户端文本/二进制 | 忽略（debug 日志；输入通道是第三刀） |
| 断开 | info 日志，连接资源随任务结束释放（无泄漏） |
| WS 线程异常/端口占用 | warn 一次，终端世界照常运行（线程隔离） |

## 6. 版本策略

v1 为全量 JSON 每拍一帧。预留演进（不在本版实现）：
- delta 快照 / 二进制编码（实体数增长或拍频提高后再做）
- 插值渲染参数（第二刀：客户端按到达帧之间插值画）
- 输入回传协议（第三刀：客户端 → 服务器意图流，接 input.rs 单入口）
- 预测回滚（第四刀：客户端预测 + 服务器快照校正）
