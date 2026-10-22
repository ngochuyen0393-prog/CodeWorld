# D-018-R1 双审修复小刀 · 施工报告

- 日期：2026-10-22
- 基线：master（9cee29b + 3d4b417），测试基线 1074
- 任务书：.cc-scratch/TASK-d018-r1-fix.md
- 结论：**P1 + P2-A/B/C/D/E/F 全修**；G1-G6 全过；**G2 实测额外抓出并修复 2 处审查未发现的照抄必死缺陷**（见 §四）

---

## 一、改动文件清单（9 个，全部列明）

| 文件 | 修复项 | 性质 |
|---|---|---|
| docs/newbie-first-thrower.md | P1（4 处） | 教学模板修至真实可抄 |
| bin/codeworld/static/index.html | P2-A | simTrajectory 一带（未碰 3D/genshin-toon 层） |
| bin/codeworld/src/main.rs | P2-B + P2-F | 落空日志/_base_speed 标注/接触遍历排序 |
| bin/codeworld/src/input.rs | P2-B | Shoot 注释退役口径 |
| docs/net-protocol.md | P2-C | shoot 退役口径统一 |
| docs/progress.md | P2-D | 冲突标记残留删除 + R1 进度追加行 |
| crates/core/src/collision.rs | P2-E | PROJECTILE_CODE/is_projectile 注释口径 |
| crates/physics/src/default.rs | P2-E | step/测试构造器/两处测试注释口径 |
| bin/codeworld/src/net.rs | P2-E | entity_kind 注释口径 |

未跟踪工作件（不入库，照 .cc-scratch 惯例）：`.cc-scratch/d018-count-shell-len.js`（字节实测脚本）、本报告。临时验证测试 `crates/arena/tests/d018_doc_wat_verify.rs` 已验证后删除（tests 目录复归空）。

---

## 二、P1：投掷器模板「照抄必死」修复

1. **spawn import 17→19 参**：文档 L21 补 D-009 的 liquid_density/liquid_drag 两个 f64，19 参序列与 sandbox.rs:875 FuncType、api.md:185 逐位一致。
2. **spawn 调用 17→19 实参**：末尾补 `(f64.const 0.0)` ×2（非液体默认），19 参顺序逐参加编号注释（1 code_ptr … 19 liquid_drag），照 api.md:185 核对。
3. **code_len 661→676（不写死拍脑袋，实测）**：
   - 口径定案：code_len = data 段**解码后**字节数（去 `\00` 哨兵）——世界从内存读的是转义还原后的原文，world NPC 同口径（main.rs `CTRL_SHELL_WAT.len()`，hunter 测试全绿即活证）。
   - 脚本 `.cc-scratch/d018-count-shell-len.js`：解码 WAT 字节串（`\"`→0x22/`\\`→0x5C/`\t\n\r`/`\hh` 两位十六进制），算法注释载明转义规则。输出：转义文本字面 719 / 解码含哨兵 677 / **code_len 实测 676**（与审稿人逐字节计数一致）。
   - §4 坑 1 文案同步纠正（原文"转义后的字节数≠原文长度"暗示数转义文本——错误口径，改为"数解码后原文、哨兵不算"）。

## 三、P2 批

- **P2-A 前端镜像（index.html simTrajectory）**：积分器半隐式（`x+=vx`）→ **中点位移**（定律前速度快照 `(vx0+vx)/2`），与 main.rs solve_launch_vector simulate_path 同式（世界物理已换轨，#028 镜像铁律）；注释 step_projectiles → step 冲量路径；PROJ_MAX_AGE 语义改"解算模拟步数上限（=CTRL_SOLVE_MAX_TICKS）——#045 后无统一弹寿命，造物弹自管 life()，弹幕总量上限=实体上限 64"；PROJ_BODY/PROJ_MASS 注释同步定装造物弹口径。只动 simTrajectory 一带。
- **P2-B 陈旧文案**：main.rs 落空日志改指 docs/newbie-first-thrower.md（spawn 造物弹带出生初速）；input.rs Shoot 注释"env.shoot 才是自定义弹道"→"自定义弹道=spawn 出生初速（env.shoot 已随 #045 退役）"；solve_launch_vector `_base_speed` 加占位注释（速度梯内嵌后此参被忽略，保留免翻十余处调用面，前端镜像同构无此参）。
- **P2-C net-protocol.md**：L132 消息示例注释/L143 shoot 行全重写（定装 spawn 造物弹口径）/L249 v1.8 条目标注"#045 已停产"/L261"shoot 第 6 参"改为历史注记；版本史末尾（v2.4 后）新增**退役口径总条**（非协议变更、上行下行零变更、历史条目以本条为准）；顺带删除预留演进里与 #045 痕迹退役矛盾的"发射痕迹进快照"一条。
- **P2-D progress.md**：第 9 行孤立 `>>>>>>> a1804fc` 冲突标记删除；追加 R1 小刀进度行。
- **P2-E 兼容口径统一**（6 处）：collision.rs ×2 / default.rs ×3 / net.rs ×1，统一为"**仅兼容历史快照反序列化，恢复即清场（弹丸不跨重启，main() 按哑弹 retain 清场），运行时无活旧弹丸**；排除语义=防御性守卫+测试构造兼容锚"——与 main() 快照恢复实际行为对齐，消灭"旧弹丸照飞照落"的矛盾说法。保留不动的历史注记：default.rs:1300 shooter 豁免退役（纯历史事实）。
- **P2-F 接触遍历确定性**：touching_now HashSet 无序遍历 → 收集 Vec 按 (a.0, b.0) 内层 Uuid 升序排序后遍历（EntityId 无 Ord 走内层 Uuid；detect_touching 已保证 a.id<b.id，排序键即全序）——#044 确定性，多接触同拍回调序固定。attack.rs 死路径按任务书留下一刀，未动。

---

## 四、G2 实测的额外收获（审查之外的新发现）

写 wasmtime 真实编译验证（照抄进测试跑 `Module::new`）时，完整模板在修完 19 参+676 后**仍然炸**，连环揪出两处审查也没发现的照抄必死缺陷：

1. **单分号注释**：模板 spawn 参数注释用 `; xxx`——WAT 行注释是 `;;`，单 `;` 直接 parse error（`expected an instruction`）。原模板注释同病。全改 `;;`。
2. **投掷 then 块括号结构错位**：spawn 尾行 `))))` 把 `(then` 提前闭合，后面的 `(return))` 变成折叠 if 里的裸指令——折叠式 then 之后只认 `(else)`（`expected keyword 'else'`）。对照 main.rs hunter_wat（活证可编译）修正括号落点：spawn 尾 `)))`，`return` 留在 then 块内，`(return)))` 收 then+if。

**结论：原模板从头就没编译通过过——审查只测了 shell 字节没测完整模板。现完整模板 wasmtime 实测通过。**

## 五、Gate 逐条证据

**G1 文档模板 19 参** ✅
```
docs/newbie-first-thrower.md:21: (param i32 i32 f64 f64 i32 i32 f64 f64 i32 i32 f64 f64 i32 i32 f64 f64 f64 f64 f64)
docs/newbie-first-thrower.md:57: (i32.const 676)           ;; 2  code_len：…
```

**G2 模板可编译（wasmtime 实测）** ✅ —— 临时测试 crates/arena/tests/d018_doc_wat_verify.rs（验证后已删）输出：
```
running 5 tests
[G1] spawn import 19 参序列：在
test doc_import_is_19_params ... ok
[G2-2] code_len=676 与解码后字节数一致：通过
test doc_code_len_equals_decoded_bytes ... ok
[G2-4] 旧值 661 截断复现：编译失败（expected an instruction 类截断错误）
test old_value_661_truncates_and_fails ... ok
[G2-1] 完整模板 wasmtime 编译：通过
[G2-3] 解码后 676 字节 shell 单独编译：通过
test doc_template_compiles_in_wasmtime ... ok
test full_decoded_shell_parses ... ok

test result: ok. 5 passed; 0 failed
```
（首跑完整模板 FAILED 于单分号/括号结构——修复后全绿；负对照 661 必炸、正对照 676 必过，双向重现审稿人实测。）

**G3 前端中点口径** ✅
```
index.html:807: x += (vx0 + vx) / 2; y += (vy0 + vy) / 2; // 中点位移（step 冲量路径同款）
```
`step_projectiles` 全文 0 命中。

**G4 全量测试** ✅ `cargo test --workspace`：
```
446+132+233+103+72+30+26+16+6+5+5 = 1074 passed; 0 failed（2 ignored = 桥 e2e，与基线一致）
```

**G5 clippy 本刀 hunks 零新警告** ✅ 全 workspace 6 条警告逐条定位，全部在既有代码区、与本刀 hunks 零交集：
```
crates/physics/src/liquid.rs:381（D-009 既有）/ crates/core/src/mask.rs:448（既有）/
bin/codeworld/src/main.rs:4736、7679（既有）/ avatars.rs:501（既有）/ net.rs:1232（既有）
```

**G6 修复项逐一 grep 证实** ✅
```
bin/codeworld/src/main.rs:3031: "…远程打击=spawn 造物弹带出生初速，模板见 docs/newbie-first-thrower.md"
bin/codeworld/src/main.rs:2689: // _base_speed：定装基准速历史占位…
bin/codeworld/src/input.rs:118: …env.shoot 已随 #045 [退役]
docs/net-protocol.md:314: - **shoot/弹丸退役口径（#045，D-018…）** / :132/:249/:262 逐处改写
docs/ >>>>>>> a1804fc 零命中（P2-D）
crates/… "恢复即清场" 统一口径 5 处命中 + net.rs 1 处（P2-E）
```

---

## 六、验证方法备注

- cargo 转发器：`node C:/Users/Administrator/AppData/Local/Temp/cc-cargo.mjs`。
- 审稿人的 wattest 仓在工作区外（沙箱禁入），验证法照抄落为项目内临时集成测试（arena 依赖现成 wasmtime，增量编译 6s），跑完即删。
- 字节实测脚本可复跑：`node .cc-scratch/d018-count-shell-len.js`（改了模板代码后重数就靠它）。

## 七、遗留（下一刀候选，本刀未动）

- attack.rs projectile_strike / grace_owner 死路径清理（任务书明示留下）。
- 4+2 条既有 clippy 警告（非本刀范围，liquid.rs 那条 D-009 落的）。
