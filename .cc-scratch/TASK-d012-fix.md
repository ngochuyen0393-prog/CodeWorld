# TASK D-012 收尾刀：双审 P1×2 + 快赢 P2（小刀，快速完成）

[Superpowers：test-driven-development——修前先写红测试。]

D:/projects/CodeWorld，D-012 力场已入库（commit dc74819）。独立双审结论：无 P0，2 条 P1 + 5 条 P2。本刀只修下列条目，**禁止扩散到其他改动**。

## P1-1：field_pulse 跨源应用序不确定（违 #044 确定性红线）

- 位置：bin/codeworld/src/main.rs collect_field_pulses（约 :1804）用 `sandbox_map.sandboxes.values_mut()`（HashMap 迭代序）收集脉冲；apply_field_pulses（约 :1822）按收集序逐条应用+极速钳制。两沙箱同拍脉冲命中同一实体时结果随 HashMap 序漂移（实例：v=(20,0) 先 +10 再 -15 → 10；先 -15 再 +10 → 15）。
- 修法：apply 前 `pulses.sort_by_key(|p| p.source.0)`（对照同文件 thrust 的 moves.sort_by_key 约 :1926 先例）。
- 测试：两脉冲同拍命中同一 dynamic 造物，断言结果与插入顺序无关（构造两个 source id，正反两序应用结果一致）。

## P1-2：场信箱 64 槽与可调 max_entities 脱钩（静默截断）

- 位置：crates/arena/src/sandbox.rs:51 FIELD_MAILBOX_CAP=64 注释断言"实体上限 64 恒够"；但 max_entities 是 CODEWORLD_MAX_ENTITIES 可调（config.rs），>64 时 call_on_field 里 `ids.len().min(FIELD_MAILBOX_CAP)` 静默截断无 warn，模块无法区分"恰 64"与"被截"。
- 修法（最小）：截断发生时 tracing::warn 一行（含实际 ids 数与 cap），api.md on_field 节加一句"清单最多 64 条，超出截断有 warn 日志"。（不做 cap 推导——那是结构改动，超出收尾刀范围。）
- 测试：构造 65 实体域内清单走 call_on_field，断言 n=64 + warn 不 panic（若 65 实体测试太重，直接单测 min 截断逻辑加日志分支即可，说明取舍）。

## P2 快赢（三条，顺手修）

1. clippy --all-targets 新增 1 条：main.rs:10195 needless_borrow（新测试 field_demo_example_compiles_and_declares_field 内 `.call_on_field(&mut handle, ...)` handle 已是 &mut）——去掉多余 &mut。修后 `cargo clippy --workspace --all-targets` 不得出现本刀新警告（存量 5 条非本刀的不管）。
2. net.rs build_snapshot 的 fields 块补排序（`sort_by_key(|f| f.id)` 或等价，与 entities 定序纪律一致）。
3. fields.rs:88 日志字段失真：`fields_covered = fields.iter().len()` 报的是清单总场数非覆盖数——改为把 covered 计数传出循环或改名字段为 fields_total（诚实命名）。

## P2 不修（记录即可，写进报告）

- 场信箱残留旧 id（契约已传 n，防御性写 0 越界风险大于收益——不动）
- on_field 整数粒度表达力（协议设计取舍，非 bug）：api.md on_field 节补一句"应答 k 为整数粒度（i32），小数级 delta 靠出生声明"（这是文档修正，要做）。

## Gate

1. `node $LOCALAPPDATA/Temp/cc-cargo.mjs test --workspace` 全绿（当前基线 1008，我方独立实测；CC 报告 1000 是提交时刻数，其后有兄弟 commit）
2. `node $LOCALAPPDATA/Temp/cc-cargo.mjs clippy --workspace --all-targets` 无本刀新增警告
3. commit（`fix(physics): D-012收尾——脉冲跨源定序+信箱截断warn+快赢P2`），逐文件 add
4. 报告追加写 `$LOCALAPPDATA/Temp/cc-report.md` 末尾（新增「收尾刀」一节：修了什么/测试数/commit hash）

## 已知信息（不需要查）

- cargo 转发器：node $LOCALAPPDATA/Temp/cc-cargo.mjs（参数原样转发）
- PendingFieldPulse 结构在 crates/arena/src/sandbox.rs（field_pulse import 闭包附近）；apply/collect 在 main.rs 搜 `field_pulse`
- FIELD_MAILBOX_CAP 与 call_on_field 都在 sandbox.rs（搜 FIELD_MAILBOX_CAP）
- thrust 排序先例：main.rs 搜 `moves.sort_by_key`
- 兄弟在途工作区有 .cc-*.md 与 Cargo.toml/sha2 改动——**不要动不要提交这些文件**（git add 只加你改的文件，逐文件 add）
- 当前 master HEAD 在 dc74819 之后已有兄弟 docs commit，直接在最新 HEAD 上做
