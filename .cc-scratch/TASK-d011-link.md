# TASK: D-011 世界内链路段——控制链+数据通道+存储块（宪法 #038 世界内段实施刀）

[Superpowers 激活：test-driven-development + systematic-debugging 文档级。]

你是 CodeWorld 项目（D:/projects/CodeWorld）的施工工程师。设计定案全文在 **docs/design-link.md（本刀的宪法，先通读）**，背景法条 decisions.md #038（通道公理）。外桥（D-001）已落地，本刀修世界内三件套：链（纵向）+消息（横向）+存储（公开块）。**全程零内容解释——世界只送信不拆信（#038）。**

## 1. core：链账本+存储块（crates/core/src/entity.rs）

- `Entity` 新增两字段（serde default，旧快照零迁移——完全照抄 binding/bound_to 先例，entity.rs:116-122）：
  - `link_to: Option<EntityId>`（serde default None）：活链造物指向母体。
  - 公开存储块：按 Entity 现有风格定（如 `pub storage: Vec<u8>` 或固定数组形态），serde default 空。随实体入快照（重启不丢=存储块比沙箱内存耐用，design-link 五节）。
- `clamp_link(v: i32) -> i32`：0/1 合法，非法钳 0+warn（照抄 clamp_binding，entity.rs:375）。
- 常量 `LINK_UNLINKED: i32 = 0` / `LINK_ACTIVE: i32 = 1`。
- 测试：serde 往返+旧快照（删字段反序列化回退）+clamp 边界——照抄 entity.rs:849-880 测试族形态。

## 2. arena：五 import + 三回调投递（crates/arena/src/sandbox.rs）

### 2.1 spawn 第 20 参 link（i32）

- FuncType 追加一个 I32（现有 19 参清单见 sandbox.rs:919-938）；spawn_call 校验链（clamp）→ PendingSpawn 加 link 字段（sandbox.rs:81+1182）。
- **19 参旧调用方全部同步**：main.rs 的 WAT 测试模板/spike/测试辅助（grep `spawn.*param.*f64.*f64.*f64.*f64.*f64` 找全）补第 20 参——D-012 加 15/16/17 参时的同款机械迁移。

### 2.2 五个新 import（全部照 bridge_send 的闭包风格，sandbox.rs:1413-1470 是样板）

```
command(ptr: i32, len: i32) -> i32     ;; 下行广播给名下全部活链造物
report(ptr: i32, len: i32) -> i32      ;; 上行沿链投母体
send(x: f64, y: f64, ptr: i32, len: i32) -> i32  ;; 横向坐标寻址（视口内门槛）
storage_write(ptr: i32, len: i32) -> i32         ;; 全量覆盖自己的公开块
storage_read(x: f64, y: f64, ptr: i32, cap: i32) -> i32  ;; 贴身读对方公开块
```

返回码语义（design-link 三四五节，api.md 同步写准）：
- command：正数=入队链数 / 0=无活链或参数坏。每拍 1 次。
- report：1=入队 / 0=参数坏 / 2=无链或断链。每拍 1 次。
- send：1=入队 / 0=参数坏 / -1=坐标无实体或不视口内。每拍 1 次。视口判定=发送方 visible 列表成员（target_visible 既有机制）。
- storage_write：N=写入字节 / -1=超容拒绝不截断 / 0=参数坏。每拍 1 次。
- storage_read：N=读到字节数 / 0=cap 不足或指针坏 / -1=坐标无实体 / -2=够不着 / -3=对方无存储（空块返回 0，-3 与 0 区分）。**贴身判据：距离 < r_self + r_target + 0.5 严格小于**（melee 同款，collision.rs:56）。读自己=距离 0 天然成立。

每拍 1 次限流：SandboxData 加 sent_flag 族（照 bridge_sent_this_tick，main.rs collect 即清——限流以拍为单位）。**与 bridge 管道分记账**：新 pending 队列（command_pending/report_pending/send_pending/storage_pending），不碰 bridge_pending/bridge_inflight。

### 2.3 三个投递回调（复用 call_on_bridge_reply 全套先例，sandbox.rs:1941-2005 是样板）

**抽一个通用投递函数**（建议 `deliver_message(handle, payload, budget) -> Result<Option<u64>>`）：has_export 检查→签名核验**先于** grow+写（P2-A 无半副作用）→ grow 分配 offset=旧末尾→写→probe_fuel 预算包裹调→缺省导出 Ok(None) 零副作用。三个回调签名：

```
on_command(offset: i32, len: i32)              ;; 造物收母体剑诀
on_report(from: i64, offset: i32, len: i32)    ;; 母体收孩子回传
on_message(from: i64, offset: i32, len: i32)   ;; 任意沙箱收横向信
```

from = 源 EntityId 低 64 位（field 信箱先例 sandbox.rs:1907，`v.0 .as_u64pair().0` 或现有写法对齐）。

## 3. bin：tick 接线+断链清算+config（bin/codeworld/src/main.rs）

### 3.1 投递相位（阶段二 a-1，外桥同相位）

tick 内 deliver_bridge_replies/collect_bridge_sends 旁新增投递步（顺序：**先投上拍入队的链消息，再跑本拍玩家代码**——消息下一拍可感知的节律与外桥回信一致；落点参照 main.rs:484 deliver_bridge_replies 的调用位）。同拍多消息按（目标,源）EntityId 升序定序（确定性）。目标死→丢弃+INFO（"无人认领"文案族）。

### 3.2 断链清算

母体死亡当拍（或既有死亡清算步旁）：查 link_to==死者 的全部造物 → link_to 清 None + INFO 一条汇总（"[link] 母体死亡，N 条链断"）。断链不杀造物（生死归 binding，电话挂了人活着）。断链不可逆。

### 3.3 spawn 接线

process_spawn_requests（main.rs:2504 set_bound_to 处旁）：link=1 → set_link_to(Some(req.creator))；link=0 → None。

### 3.4 config

`CODEWORLD_LINK_PAYLOAD_MAX`（默认 8192）/`CODEWORLD_STORAGE_CAP`（默认 4096），照 bridge_payload_max 装配先例（main.rs:126-180+1167-1179）。

## 4. 文档

- **api.md**：五 import+三回调+spawn 第 20 参——完整节（照 bridge_send/thrust 的写法：语义/返回码/限流/fuel 口径/WAT 示例）。快照注意：链消息不进快照（在途消息=当拍生命周期）。
- **decisions.md**：文首新增 #053（照 #038 格式：裁决/法条引用/落点/否决记录——否决编号寻址/链上写盘/送达回执/视口外投递/世界侧持久化五条）。
- **game-menu.md 1.7 常数总账**：两新常数入账（出厂默认+护栏性质标注）。
- **players/link-demo.wat**：风筝演示对——母体（每 8 拍 command 广播"转向方向"+收 on_report 打日志）+ 侦察弹（活链+每拍 report 自己坐标+收 on_command 改 move_by 方向）。WAT 必须真实可编译（wasmtime 实测过，同 newbie-first-thrower.md 的验收纪律）。
- **progress.md** + **dev-plan.md D-011 条目**标已落地。

## 5. 测试锚（TDD：先红后绿，每锚至少一断言，负面路径必须有）

1. 链三态：link=0 放生（command 返回 0）/link=1 活链（command 扇出 N 个孩子）/母体死→断链（link_to None+造物活）。
2. 广播扇出：一母三子，command 一次三子全收（投递顺序=EntityId 升序断言）。
3. report 沿链：孩子 report→母体 on_report 收到，from 低 64 位正确。
4. **夺剑反杀**：攻陷造物（决斗会话换魂）后链仍在（link_to 不变）+report 照投原母体（#038 预言的涌现验证锚）。
5. send 视口门槛：视口内命中/视口外 -1/坐标空 -1。
6. storage 贴身判据：够不着 -2/贴身成功/超容 -1 拒绝不截断/空块 0 vs 无块 -3。
7. 缺省导出：对方无 on_message → 入队成功投递作废零副作用+INFO。
8. 快照旧档：删 link_to/storage 字段的旧快照反序列化回退 None/空。
9. 限流：同拍第二次 command 返回 0（或复用节律码）+下一拍恢复。
10. 燃料：投递烧 probe_fuel 预算+接收方主配额不动（照 call_on_bridge_reply_restores_fuel 先例形态）。
11. e2e：spawn link=1 → 母体 command → 下一拍孩子 on_command 收到（两拍节律断言，与外桥 e2e 同款）。

## Gate（全过才算完）

- G1 全量 `cargo test --workspace` ≥1092 绿 0 failed（亲跑，awk 累加 test result 行报数）。
- G2 clippy 本刀文件零新 warning。
- G3 grep 证据：entity.rs 有 link_to+clamp_link；sandbox.rs 有五个新 import 注册+deliver_message 通用投递；main.rs 有断链清算+两 config 常数；api.md 有五 import 节；decisions.md 有 #053；menu 1.7 有两常数；players/link-demo.wat 存在。
- G4 link-demo.wat 真实编译跑通（wasmtime 加载或 e2e 测试覆盖）。
- G5 单 commit，消息 `feat(core,arena,bin,docs): D-011 世界内链路段——控制链广播+链路回传+数据通道+存储块（#038 世界内段）`。只 add 本刀文件。
- 报告：.cc-scratch/cc-report-d011.md（改动/测试输出/G1-G5 证据/经验教训/失败路径）。

## 已知信息（不需要查，直接用）

- cargo 转发器：`node $LOCALAPPDATA/Temp/cc-cargo.mjs <args>`（PATH 丢 cargo 是常态）。
- call_on_bridge_reply（sandbox.rs:1941）=投递回调全套先例：签名核验先于 grow+写（P2-A 无半副作用）、grow offset=旧末尾、probe_fuel 预算包裹、缺省导出 Ok(None) 零副作用。
- bridge_send（sandbox.rs:1413）=import 注册样板：api_calls 计数+返回码+限流 flag+INFO 日志。
- EntityId=Uuid（entity.rs:14）；低 64 位写法见 field 信箱 sandbox.rs:1907；bound_to/binding 的 serde default+clamp 先例 entity.rs:116-122/375。
- spawn 19 参现状：sandbox.rs:919-938 FuncType；PendingSpawn sandbox.rs:81；spawn_call 校验链 sandbox.rs:1040-1110；main.rs:2504 set_bound_to 旁是 link 接线点。
- 投递相位：deliver_bridge_replies main.rs:484 调用位；collect 先清 sent_this_tick（main.rs:579）。
- visible 列表=视口判定既有机制（main.rs:201-212）；melee 判距严格 <（collision.rs:56）。
- 常量以代码现实现为准，别信本任务书记忆。master 基线 c5c9c86，测试 1092 绿。
- 报告出现 ≠ 收尾：commit 后才算完。
