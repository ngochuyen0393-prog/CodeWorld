# TASK（期四第一刀，中等任务）：WebSocket 服务 + 每拍状态快照推送

## ⚠️ 必须使用 test-driven-development（红→绿）

## 宪法依据

decisions.md #022（双层模型）/ #024（操控身份）/ #026（本任务写入：期四浏览器化定案——薄客户端/Screeps官方同构/快照推送+插值路线/原生Canvas零框架）。架构对齐 world-foundation.md（世界是唯一权威，浏览器只是显示器+遥控器）。

## 业界对标（本刀对应组件）

authoritative server + snapshot broadcast（Screeps 官方客户端/SnapNet/Valve Source 网络文档）。第一刀只做"推"，插值/预测是第二/四刀。

## 项目上下文

项目：D:/projects/CodeWorld（Rust workspace，基线 536 测试全绿，git e533b8b）
现状：
- 主循环：main() tick 循环（终端 ANSI 渲染并行存在）——WebSocket 推挂同一循环，不替代终端
- tokio 已是 workspace 依赖（features full）
- 渲染数据源：world.entities（id/pos/appearance glyph+color/alive）+ caption（字幕）+ tick
- input.rs 意图路由器（键盘来源；本刀不动它——第三刀才接 WebSocket 输入）
- Config 模式：CODEWORLD_* 环境变量（WS 端口等新增照抄模式）
- cargo 转发：node C:/Users/Administrator/AppData/Local/Temp/cc-cargo.mjs（不存在则重建）

## 需求

### A. WebSocket 服务（tokio + tokio-tungstenite 或 axum/ws——自选并说明，倾向 axum：路由/静态文件服务一把抓，第二刀的 HTML 就能它伺服）
1. 新增 bin 或模块：WS 服务器与 tick 循环并行（std 主循环 + tokio runtime 线程，channel 桥接——tick 侧每拍把快照发给广播通道，WS 侧广播给所有连接）
2. CODEWORLD_WS_PORT（默认 8080；0=禁用 WS）；监听失败 warn 不崩（终端体验不受影响）
3. 连接管理：多客户端并发（观战多人）；断开清理；连接/断开 info 日志

### B. 快照格式（第一版全量 JSON，每拍一帧）
```json
{
  "tick": 42,
  "caption": "tick=42 | ... | ctrl:P",
  "viewport": {"x0": -50, "y0": -25, "w": 100, "h": 50},
  "entities": [
    {"id": "uuid", "glyph": "A", "color": "red", "x": 12.5, "y": 0.0, "kind": "npc"},
    {"id": "uuid", "glyph": "P", "color": "green", "x": 35.0, "y": 0.0, "kind": "player", "name": "pioneer", "controlled": true},
    {"id": "uuid", "glyph": "T", "color": "cyan", "x": 40.0, "y": 0.0, "kind": "creation", "owner": "uuid"}
  ]
}
```
- kind 判定：code_hash（attacker/defender/hunter/spy=npc；players 目录名=player；creation=creation）——数据已有，纯映射
- controlled=true：当前 me 声明的操控目标（#024 的 controlled_target 结果）
- viewport 传世界坐标→画布坐标的换算基准
- 序列化用 serde_json（已在依赖）；字段名稳定（第二刀前端依赖它）
- 实体数≤64，全量每拍 JSON 也就几 KB——不做过早优化（delta 留以后，快照先对）

### C. tick 桥接（std ↔ tokio）
- 主循环 tick 末尾（渲染步骤旁）组装快照 → mpsc/broadcast channel → WS 线程广播
- WS 禁用时零开销（不组装不发）
- WS 线程挂了不影响主循环（catch+warn）
- 收到客户端消息：第一刀只回 pong/忽略（输入是第三刀）——但框架上消息接收循环要立好

### D. 验收工具页（本刀最小前端）
- 一个 static/index.html（axum 伺服或简单 HTTP），无框架纯手写 JS：
  - 连 WS → 收快照 → console.log 前几帧 + 页面上滚 tick 计数和实体数
  - 目的：土豆浏览器开 http://localhost:8080 能看到世界在跳（验收物）
- HTML 存 bin/codeworld/static/index.html（或 web/ 目录，自定并报告）

### E. 文档
- decisions.md #026（期四定案：薄客户端架构/快照格式 v1/插值预测路线图）
- api.md 或新 docs/net-protocol.md：快照 JSON 字段表 v1（前端契约，第二刀依赖）
- progress.md 更新

## 验收标准

1. 新测试：快照序列化（实体映射三 kind/controlled 标记/viewport 域）；channel 桥（tick 后快照可被取到）；WS 禁用=零开销路径；端口配置解析
2. WS 集成测试：起服务器→连测试客户端→收到的帧是合法 JSON 且 tick 递增→断开清理（tokio-tungstenite 客户端即可测）
3. 536 基线零回归（WS 关闭时四生物剧本/终端行为完全不变）
4. 冒烟（写入报告步骤供土豆复验）：start.bat 启动（需带 WS）→ 浏览器 localhost:8080 → 看到 tick 跳动+实体数+console 有快照帧；终端画面同步正常
5. clippy/check 三连过
6. #026/net-protocol.md/progress 同步

## 已知信息（不需要查）

- tokio 已在 workspace deps（full）；axum/tokio-tungstenite 需新增（走 rsproxy 镜像，任选但报告写理由）
- tick 循环是 std（非 async）——桥接用 std::sync::mpsc 或 tokio sync broadcast（跨线程），实现自定，简单优先
- caption 的当前值获取：Renderer 有 caption_line() 观测访问器（#023 已建）
- controlled_target 在 players.rs（每 tick 调用点在 tick_with_intents）——快照组装处可复用同结果
- Windows 防火墙可能弹第一次监听授权——冒烟步骤里提示土豆允许
- 静态页 JS 不引任何 npm 依赖（手写，与项目零前端工具链一致——第二刀 Canvas 也手写）

## 硬 Gate

```bash
cd D:/projects/CodeWorld && cargo test --workspace
cd D:/projects/CodeWorld && cargo clippy --workspace --tests -- -D warnings
cd D:/projects/CodeWorld && cargo check --workspace
```

## 审查（单审一轮）

聚焦：WS 线程与主循环隔离（挂不死世界）；快照字段与 net-protocol.md 契约一致；多客户端广播正确；禁用路径真零开销；防火墙/端口冲突的降级体验。

## 交付物

1. 中文 commit（feat: 期四第一刀——WebSocket快照推送+验收页）
2. C:/Users/Administrator/AppData/Local/Temp/cc-report.md（简版三节）

不要弹选项；信息缺失超3次试错即停写报告。
