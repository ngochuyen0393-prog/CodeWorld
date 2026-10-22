# TASK: D-018-R1 双审修复小刀——投掷器文档模板 19 参/字节实测 + 前端镜像同步 + 3 处 P2

[Superpowers 激活：verification-before-completion 文档级。]

你是 CodeWorld 项目（D:/projects/CodeWorld）的施工工程师。D-018 弹丸退役工程主体已落地（commit 9cee29b+3d4b417，master，1074 绿），独立双审抓出 1 个 P1+若干 P2。本刀全部修复。**当前 master 树已有 D-009 水体（162fd82：spawn 已是 19 参，新增 liquid_density/liquid_drag 两个 f64 尾参）**——文档模板必须按 19 参写。

## P1（必修）：docs/newbie-first-thrower.md 模板照抄必死

1. **spawn import 17 参 → 19 参**：文档第 20-21 行 `(param i32 i32 f64 f64 i32 i32 f64 f64 i32 i32 f64 f64 i32 i32 f64 f64 f64)` 缺 D-009 的 liquid_density/liquid_drag。服务器 FuncType 已 19 参（crates/arena/src/sandbox.rs:875，api.md:185 签名同步照抄）。**WAT 模板的 spawn import 声明与调用都补两个 f64.const 0.0（非液体默认）**——玩家模板必须能在当前世界真实跑通。
2. **code_len=661 错**：实际子弹代码 676 字节（审稿人逐字节数出+wat parse 实测）。修法：**不写死数字**——写一个 node 小脚本（.cc-scratch/d018-count-shell-len.js）从文档 data 段转义文本算出准确字节数填入，并在文档把 661 改成实测值；脚本的算法注释里写清 WAT data 段转义规则（\\00 结尾、转义序列按字面）。**验收：改完后用 wasmtime/wat 实际编译文档里的完整 WAT 模板（照抄进临时 .wat 文件 parse）必须通过**——审查已实测旧值 661 截断报 unknown operator、676 通过；你修完要重现这个验证（可写 .cc-scratch/d018-verify-doc.js 或直接用项目测试二进制/wattest 同款法）。
3. 顺带：模板内 spawn 调用参数注释逐个核对一遍（19 参顺序照 api.md:185）。

## P2（全修，小刀快回）

- **P2-A 前端镜像同步（中）**：bin/codeworld/static/index.html simTrajectory（~L793）仍是半隐式欧拉+PROJ_MAX_AGE=30+注释提 step_projectiles。改：积分器同步中点口径（与 main.rs solve_launch_vector 的 simulate_path 同式——世界物理已换轨，#028 镜像铁律）；PROJ_MAX_AGE 语义改"通用弹幕上限 64 实体/弹自管寿命，30 仅模拟上限"或直接对齐 solve 口径；注释改指 step 冲量路径。**注意 index.html 是 D-004b 刚大改过的文件（genshin-toon），只动 simTrajectory 一带，别碰 3D 层**。
- **P2-B 陈旧文案**：main.rs:3022 落空日志"远程打击请用 shoot 弹丸"→改指 docs/newbie-first-thrower.md；input.rs:118 注释"玩家代码 env.shoot 才是自定义弹道"同改；solve_launch_vector 未用 _base_speed 参数（main.rs:2682）删掉或标注。
- **P2-C net-protocol.md 同步**：L132/143/249/261 shoot 仍在役描述→退役口径（一段话：#045 退役，操控射击=定装 spawn 造物弹，自定义弹道看 newbie-first-thrower.md）。
- **P2-D progress.md 冲突标记残留**：第 9 行孤立 `>>>>>>> a1804fc` 删掉。
- **P2-E 兼容口径统一**：collision.rs/net.rs/physics 兼容测试注释里"旧弹丸实体照飞照落"的说法与 main() 快照恢复即清（retain(!is_projectile)）矛盾——统一口径为"仅兼容历史快照反序列化，恢复即清场，运行时无活旧弹丸"。
- **P2-F（顺手，可不修但建议）**：main.rs 接触遍历 touching_now HashSet 无序（:1818-1829）——按 EntityId 排序后遍历（#044 确定性，多接触同拍回调序固定）。attack.rs projectile_strike/grace_owner 死路径清理留下一刀，本刀不动。

## Gate

- G1 文档模板 19 参：grep 文档 import 行含 `f64 f64 f64 f64 f64` 尾五 f64（blob 后 field 三 f64+liquid 两 f64 的完整 19 参序列）。
- G2 文档模板可编译：临时 wat 文件实际 parse 通过（验证脚本+输出贴报告）。
- G3 前端：simTrajectory 用中点口径（grep `0.5` 中点系数/或同式代码），不再引用 step_projectiles。
- G4 全量 cargo test --workspace ≥1074 绿 0 failed。
- G5 clippy 无新 warning（本刀 hunks）。
- G6 修复项逐一 grep 证实（P2-B/C/D/E 各一行证据）。

## 已知信息（不需要查，直接用）

- cargo 转发器：`node $LOCALAPPDATA/Temp/cc-cargo.mjs <args>`。
- master 已含 D-009：spawn 19 参（api.md:185），测试基线 1074。
- index.html 的 3D/genshin-toon 段是 D-004b 刚落的，禁碰。
- git：一次 commit，消息 `fix(bin,web,docs): D-018-R1 双审修复——投掷器模板19参+字节实测、前端镜像中点口径、P2批`。只 add 本刀文件。
- 报告：.cc-scratch/cc-report-d018-r1.md（修复清单/验证输出/G1-G6）。
- node 可用（v26.7.0）；wattest 同款验证法可用 cargo 小实验仓（审稿人在 C:/Users/Administrator/AppData/Local/hermes/cache/scratch/wattest/ 留了一个，可直接用或照抄法子）。
