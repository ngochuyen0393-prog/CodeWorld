# D-051a 皮入沙箱 · R2 确认审（需求镜头）审查报告

> 审查员：Hermes（worker t_6d70ed33）· 2026-10-22 · 只读审查，未改任何仓库文件
> 需求基准：卡面五点 + `.cc-scratch/cc-report-d051a.md` + decisions.md #051/#038/#043
> 审查对象：四笔提交链 cbf609c → 2952db7 → 3721e69 → 198fbed（R1 返工已落地）
> 纪律：每条结论均带本会话亲验证据（文件:行号或亲跑测试输出），未采信报告转述。
> 注：本路径原有一份先前审查稿，本版为独立复审重写，两版结论一致（五点全 SATISFIED）。

---

## 一、卡面五点逐条核对

### 1. 沙箱资产段 —— **SATISFIED**

**验证点：SandboxData.assets + Entity.assets 双处镜像 ✅；资产段无任何燃料扣减路径 ✅；配额是"盒子容量"不是内存执行 ✅**

- **双处镜像**：
  - `crates/arena/src/sandbox.rs:276-281`：`SandboxData.assets: Vec<String>`（装戴清单，注释明写「数据非执行：不烧 fuel、不参与编译实例化」「与 birth_blob 同生命周期（随沙箱存活，不持久化，换血时随行转移——replace_sandbox_assets）」）
  - `crates/core/src/entity.rs:170-175`：`Entity.assets: Vec<String>`（serde default 空=旧快照兼容，radius/mass 先例；注释明写「与沙箱 SandboxData.assets 镜像一致（换血随行语义见 arena replace_sandbox_assets）」）；构造器初始化空清单（entity.rs:361），测试 `new_entity_starts_with_empty_assets`（entity.rs:1023-1027）锚定
- **同居语义**：资产段长在 SandboxData 里（与 birth_blob/pending_attacks 同一 store 数据体），与代码（Module/Instance）、数据同箱；`replace_sandbox_assets_empties_old_sandbox`（sandbox.rs:2766-2778）锚定移动非复制防双重持有
- **数据非执行零燃料路径**（结构性亲验）：assets.rs 全文 194 行零 fuel 引用；`create_sandbox`（sandbox.rs:1647-1661）fuel 只从 wasm 参数设置不读 assets；`replace_sandbox_assets`（sandbox.rs:1718-1725）纯 `std::mem::take` 移动 Vec 全程无 fuel 触点；配额判定（assets.rs:72-94）纯 saturating 字节算术。不是执行约束
- **「盒子容量」口径**：assets.rs:3-8 模块头明写「配额账管的是'盒子里装多少东西'，64MB 内存上限管的是'魂的房间多大'——两本账并行不混」（R1 两源口径）

### 2. 资产配额 —— **SATISFIED**

**验证点：SANDBOX_ASSET_QUOTA_BYTES 常数 ✅ + game-menu 1.7 总账行 ✅ + 超限整次拒绝不裁剪测试 ✅**

- `crates/arena/src/assets.rs:19`：`pub const SANDBOX_ASSET_QUOTA_BYTES: usize = 1024 * 1024`（1MB，注释明写 Screeps 5MB 思路、常数入 game-menu 1.7 总账）；`default_limit_is_one_megabyte` 测试锚定（assets.rs:115-124）
- `docs/game-menu.md:109`（§1.7 起于 :72）：「单沙箱资产配额 | 皮入沙箱的盒子容量（引擎+皮入配额账；魂走 64MB 内存上限独立账，两账并行…）| 1MB（SANDBOX_ASSET_QUOTA_BYTES，CODEWORLD_ASSET_QUOTA 覆盖）| ⚠️ 待土豆拍板」——与 assets.rs 模块头两源口径逐字一致
- **超限整次拒绝不裁剪**：
  - 模块头三处明写（assets.rs:11,51,81「整次拒绝不裁剪，不落账」）；arena 层 `admit_over_limit_rejects_with_diff`（assets.rs:143-150）Err 携带超额字节（1100-1000=100）
  - bin 端到端 `replace_code_rejects_when_payload_plus_assets_exceed_quota`（main.rs:6333-6360）✅ 绿：超配额 → 拒绝换血，**旧沙箱仍在且清单原样**（:6355-6360「拒绝换血后资产清单应原样保留（不丢）」）
  - 拒绝时序保证：校验在 remove 之前（main.rs:860-863「旧沙箱从未离开映射表，资产天然不丢」）——不裁剪不丢皮有结构性保证
- bin 侧可调：config.rs asset_quota 字段 + CODEWORLD_ASSET_QUOTA 覆盖（0/非法回退默认）
- R1 无状态化落地亲验：assets.rs:21-31「无状态：每次按实况校验，不落账」，落账字段已删，admit/admit_swap 共用 check 核心（:55-68,72-86），`admit_and_admit_swap_same_semantics`（:173-185）锚两入口 Ok/Err/差额全等

### 3. 夺取语义接线 —— **SATISFIED**

**验证点：replace_code 路径中资产转移无特殊分支 ✅、随行日志 ✅、失败/超限=旧沙箱原样保留 ✅**

- **无特殊分支**：资产转移内置于 arena `replace_sandbox` Ok 分支**无条件调用**（sandbox.rs:1695-1698「#051 夺沙箱=夺全套：资产段随行（在拆旧前转移——失败路径根本进不了这里，旧沙箱随 Err 原样带回，资产自然不丢）」）——不区分攻击者/被攻击者/来源，谁换血资产都随行。#043 身份链复用亲验：entity_id 从旧沙箱读出传给新沙箱（sandbox.rs:1682-1684）身份延续，资产自动跟随，零 if 判"该不该转移"；bin 侧 replace_code（main.rs:852-967）对资产零额外分支
- **随行日志**：bin 层 `[skin] 资产随行` INFO（main.rs:936-944，非空才打）；arena 层 `[skin] 沙箱资产段随行转移（夺沙箱=夺全套）` debug（sandbox.rs:1722-1724）；销毁侧 `[skin] 资产随沙箱消亡`（main.rs:313-321，带皮销毁留痕）
- **失败/超限=旧沙箱原样保留**：
  - arena 层 `replace_sandbox_failure_keeps_assets_in_old_sandbox`（sandbox.rs:2727-2743）：payload 非法 → Err 带回旧沙箱，资产清单原样
  - bin 层双闸均在校验→remove 之前：①fail-closed id 核实门（main.rs:877-884，`[skin] 换血拒绝：资产 id 不在镜像仓…（fail-closed）`）②配额门（main.rs:890-901）
  - R1 新增 `replace_code_rejects_when_asset_id_missing_from_mirror`（main.rs:6417-6441）✅ 绿：id 查不到镜像仓 → 拒绝且旧沙箱资产原样；端到端正面 `replace_code_transfers_assets_to_new_sandbox`（main.rs:6365-6392）✅ 绿：全链路后新沙箱读得到完整清单

### 4. 镜像分发 —— **SATISFIED**

**验证点：net.rs EntitySnapshot.asset_ids 恒有字段 ✅；index.html skinCache/ensureSkin/失败回落 ✅；AssetMirror by_sha 去重 ✅**

- **快照恒有字段**：`bin/codeworld/src/net.rs:175-179` `pub asset_ids: Vec<String>`（注释明写「恒有字段空数组（空盒省字段是优化，恒有=前端零可选分支，同 attacks/fields 先例）」）+ `:240` `asset_ids: e.assets.clone()` 投影；测试 `snapshot_asset_ids_empty_array_always_present`（net.rs:786-798）✅ 绿锚定空数组恒有
- **前端首见取图**：`bin/codeworld/static/index.html:295` skinCache Map（id→{img,state}）；`:365-372` 快照到达触发 ensureSkin（首见拉图，已见复用缓存）；`:415-423` ensureSkin 向 `/assets/avatars/{id}.png`（D-017 ServeDir 同源=世界侧镜像）拉图，onerror 永久置 failed 本会话不再重试
- **替身兜底**：`:1157-1168` 绘制层仅 `state==='ready' && naturalWidth>0` 才 drawImage，否则 continue 回落既有默认圆+glyph 路径（存量路径零改动=兜底零新代码路径）。浏览器视觉验收归土豆（环境权限边界，报告 §3 场景③同口径）
- **sha256 去重**：`bin/codeworld/src/mirror.rs:48` `by_sha: HashMap<String, String>`（sha256→首个登记 id 的内容寻址去重账，MC 同款）+ `:95-109` insert 同指纹复用内容账（id 层条目恒建）；判据测试 `from_registry_dedups_same_content_to_one_copy`（mirror.rs:253-275）✅ 绿：两 id 同图 → id 层全量可查（len==2、双双查到大小）而 `unique_contents()==1`；对照组 `from_registry_counts_distinct_contents_separately`（:277-287）锚"去重不是合并"

### 5. 改皮写口 —— **SATISFIED**

**验证点：换血后新代码可改写资产段并读回（测试锚 `replaced_sandbox_assets_writable_by_new_code`）✅**

- `crates/arena/src/sandbox.rs:2745-2763`：`replaced_sandbox_assets_writable_by_new_code` ✅ 绿——换血成功后对新沙箱资产段整表换新（测试注释原话「金龙旗改黑龙旗=占领宣示」）并读回断言；注释明写「接触通道的换血路径本身就是写口，本测锚定写口存在（…零新 import）」
- **#038 通道公理口径核实**：资产段就在 SandboxData 既有可写数据体内（pub 字段无 API 门），接触换血路径可触及即可写——**零新法、零新 API**（全 16 个 env import 清单见边界②，无一资产相关）。与卡面「零新法只是确认资产段在可写范围内」逐字吻合
- 语义文档：`docs/api.md:421-432` 五点五小节四语义表（装戴/随行/配额/改皮战术），:430「改皮零新 import（直接改沙箱资产段）」
- 测试名与卡面指定锚完全一致

---

## 二、边界核对（卡面 §1.C，未越界=符合卡面）

1. **出生装戴只挂字段未接 spawn 参数 ✅ 如实未越界**：字段已挂但 spawn 全链路零资产参数——`bin/codeworld/src/input.rs` grep "assets" 零命中；`new_entity_starts_with_empty_assets` 锚定出生清单恒空；转移测试注释明写「装戴清单由 host 写入（未来装戴刀的入口）」；entity.rs:172 注释「出生装戴由玩家声明（…后续刀）」
2. **env.spawn 未动、零新 import ✅ 如实未越界**：`git show cbf609c -- crates/arena/src/sandbox.rs` 中 func_wrap/env/spawn 注册行改动为零；全仓 import 清单亲数 16 个：log/get_pos/move_by/hover/visible_count/visible_at/attack_at/birth_blob/bridge_send/destroy/field_pulse/probe/scan/shoot/spawn/thrust——无一资产相关（field_pulse 属 D-012 自有卡面）
3. **decisions.md 未改 ✅ 如实未越界**：四笔提交文件清单逐一核对（git show --stat）——cbf609c（assets.rs/sandbox.rs/entity.rs）、2952db7（Cargo×3/config/main/mirror/net/index.html）、3721e69（api/game-menu/net-protocol/progress 四文档）、198fbed（8 文件）——**无一笔含 docs/decisions.md**。#051 已在宪（2026-10-21 入档），本刀纯工程落地，无擅自立法

---

## 三、机器验证（全部本会话亲自跑）

### 1. 全仓测试（`node cc-cargo.mjs test --workspace`，exit 0，2026-10-22 实跑）

完整日志：`D:/projects/CodeWorld/.cc-scratch/d051a-r2-test.log`。汇总原文：

```
test result: ok. 446 passed; 0 failed; 2 ignored; 0 measured; 0 filtered out; finished in 2.12s
test result: ok. 149 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.16s
test result: ok. 223 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.13s
test result: ok.  85 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.11s
test result: ok.  72 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s
test result: ok.  30 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: ok.  26 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.04s
test result: ok.  16 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: ok.   6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.02s
test result: ok.   5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.02s
test result: ok.   5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.32s
+ doc-tests ×8 全 0
```

**合计 1063 passed / 0 failed（2 ignored），零 FAILED/panic/error**。注：现 HEAD 已越过本卡顶部（含 D-012 收尾 82e0846 与 D-001b ef133ec），1063 为当前动态计数；本卡自身份额对账：arena 149 含 assets 8+sandbox 4、core 223 含 entity 3、bin 446 含 mirror 6+config 3+main 4+net 2——与报告 §2/§7 清单吻合，全绿零回归。

### 2. 五个关键测试名存在且绿（实测日志摘录）

```
test mirror::tests::from_registry_dedups_same_content_to_one_copy ... ok
test tests::replace_code_rejects_when_asset_id_missing_from_mirror ... ok
test tests::replace_code_rejects_when_payload_plus_assets_exceed_quota ... ok
test sandbox::tests::replaced_sandbox_assets_writable_by_new_code ... ok
test entity::tests::entity_assets_serde_backcompat_old_snapshot ... ok
```

### 3. 四笔提交链确认

`git log --oneline -8`：`b1b0e3b → 82e0846 → ef133ec → 198fbed → b3be411 → 3721e69 → 2952db7 → cbf609c`——**cbf609c / 2952db7 / 3721e69 / 198fbed 四笔俱在**，链序正确，均前进式提交。

---

## 四、附注（非缺陷，记录在案）

- 版本实取 **v2.4**（卡面写 v2.3，但 v2.3 已被 D-012 力场占用）——net.rs grep 复核 v2.3 零命中 ✅，net-protocol §8 版本记录注明缘由。R1 已对齐，符合口径如实。
- 配额 1MB 为临时值，game-menu 1.7 行 ⚠️ 待土豆拍板标记在位；CODEWORLD_ASSET_QUOTA 可调（0 回退默认，无"禁皮"语义）。
- R1 遗留三项不改项（mirror insert 同 id 异内容防御缺口/配额按份与空 payload 不对称语义/commit 计数勘误）均已在报告 §7 记录在案，属玩法语义留土豆终审范畴，不构成本审缺陷。
- 环境权限边界：前端替身兜底的浏览器视觉验收（报告 §5 建议三分钟人工）不在本审可执行范围，归土豆。

---

## 总结论

五点全 SATISFIED，边界三项全部如实未越界，机器验证 1063 全绿，四笔提交链完整，五个关键测试名俱在且绿，R1 返工四项（P1 fail-closed / P2 版本 / P2 两源口径 / P2 注释写实）逐一在代码与文档中亲验落地。无缺项。

**R2-REQ: PASS**

By CC.
