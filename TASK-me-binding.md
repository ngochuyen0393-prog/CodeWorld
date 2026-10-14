# TASK（轻装挡）：操控身份绑定——me 声明+观战模式（#024）

## ⚠️ 必须使用 test-driven-development（红→绿）

## 宪法依据

decisions.md #022（双层模型）+ #023（操控层v0）。本任务修正 #023 的"操控目标=第一个进场玩家"偷懒语义：操控权=身份绑定，非顺序。终态句式"输入→我的角色"从本任务成立，期四换凭证（文件声明→账号认证）路由不变。

## 项目上下文

项目：D:/projects/CodeWorld（Rust workspace，基线 529 测试全绿，git 881af0b）
现状：
- bin/codeworld/src/input.rs：意图层（Intent/key_to_intent/route_intents/输入线程/终端恢复）
- bin/codeworld/src/players.rs:176 first_online()——操控目标选取（本任务改造对象）
- main.rs tick_with_intents：操控目标查询处
- players/ 目录现有：example.wat、pioneer.wat、pioneer.wat.txt、README.md
- cargo 转发：node C:/Users/Administrator/AppData/Local/Temp/cc-cargo.mjs（不存在则重建）

## 需求

### A. 本地身份声明（me 声明）
1. `players/me.txt`：一行内容=操控者声明的玩家名（如 `pioneer`）。世界启动时读取（trim、空文件/不存在=未声明）
2. 等价环境变量 `CODEWORLD_ME` 优先于 me.txt（两者都没=观战）
3. 声明的名字必须对应已进场的在线玩家；名字无效（拼错/未进场/已下线）→ warn 日志一次 + 回落观战模式
4. 玩家中途下线（删文件）且是操控目标 → warn 一次 + 自动转观战；重新上线（文件回来）→ 不自动恢复操控（需重启或下个任务做动态恢复，MVP 从简，日志说明）

### B. 观战模式
1. 未声明/声明无效：无操控目标。按键行为：Quit 正常；移动/攻击键 debug 日志一次"观战模式无操控目标"（不刷屏），画面照常
2. 字幕 ctrl 段：观战时显示 `ctrl:--`（区分"无人"与"有人但不是你"）；操控目标下线后同样回落 ctrl:--
3. 高亮：观战时无高亮实体

### C. 操控目标解析改造
1. first_online() 语义退役：新增 controlled_target()（读 me 声明→校验在线→返回 (name, id)），main 调用点替换
2. 每 tick 解析（me.txt 中途创建/修改也能生效，读文件每拍一次成本可忽略——或加 mtime 缓存，自定，简单优先）
3. 相关旧测试语义更新（"第一个进场"→"me 声明"），报告列出改动

### D. example.wat 模板归位
1. players/example.wat 移到 players/_template.wat.txt（下划线前缀=不会被当玩家加载的模板，与 pioneer.wat.txt 同款约定；.wat.txt 扩展名目录扫描不识别）
2. players/README.md 更新：说明 me.txt 用法、模板约定（想用模板=复制改名 .wat）、"操控自己声明的角色"语义
3. start.bat 提示行加一句 me.txt（ASCII 英文：Set players/me.txt to control your character）

### E. 文档
- api.md 操控层节更新：me 声明语义表（声明/未声明/无效三态）、观战模式、期四凭证升级预告（文件声明→账号认证，路由不变）
- decisions.md #024（操控身份绑定定案：操控权=身份声明非顺序；观战=无操控输入降级；Tab切换记期四）

## 验收标准

1. 新测试：me 声明有效→操控目标=声明玩家（非第一个进场——用两个玩家文件测序）；无声明→观战（按键安全+ctrl:--）；声明无效→warn+观战；CODEWORLD_ME 优先于 me.txt；操控目标下线→回落观战 ctrl:--
2. 529 基线零回归（四生物剧本不变；无人按键行为不变）
3. 冒烟（写入报告供土豆人工复验）：players/ 只放 pioneer.wat + me.txt 内容 pioneer → 启动 → 字幕 ctrl:P、P 高亮、WASD 操控 P；删 me.txt 下局 → ctrl:-- 观战
4. clippy/check 三连过
5. api.md/#024/README/start.bat 同步

## 已知信息（不需要查）

- players 目录扫描在 players.rs（load 时过滤 .wat 扩展名）——me.txt 不影响扫描；_template.wat.txt 同理不识别
- input.rs 的 Intent/路由结构不动——只改操控目标来源（first_online→controlled_target）
- caption.rs 的 ctrl 段已有（#023），扩展 "--" 显示即可
- 每 tick 读 me.txt：std::fs::read_to_string 一行文件，成本可忽略；mtime 缓存非必需
- 高亮在 render lib.rs set_controlled——传入 Option 即可自然处理观战
- 观战按键日志防刷屏：bool once 标志（无玩家提示一次的同款模式，main.rs 已有先例）

## 硬 Gate

```bash
cd D:/projects/CodeWorld && cargo test --workspace
cd D:/projects/CodeWorld && cargo clippy --workspace --tests -- -D warnings
cd D:/projects/CodeWorld && cargo check --workspace
```

## 审查（轻装挡单审一轮）

聚焦：me 声明三态解析正确性；观战按键安全；下线回落路径；example 模板归位后 players/ 扫描不受污染；旧测试语义更新无遗漏。

## 交付物

1. 中文 commit（feat: 操控身份绑定#024——me声明+观战模式）
2. C:/Users/Administrator/AppData/Local/Temp/cc-report.md（简版三节）

不要弹选项；信息缺失超3次试错即停写报告。
