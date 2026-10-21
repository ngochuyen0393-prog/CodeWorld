# D-051a 皮入沙箱 · R2 确认审（质量镜头）

> 审查员 worker t_6d70ed33（只读）· 2026-10-22
> 基准：R1 必改四项 + cc-report-d051a.md §7/§8 · 返工 commit `198fbed`
> 方法：逐文件人工核对（HEAD 当前态）+ diff 漂移归属 + 机器验证亲跑

---

## 一、R1 必改四项逐项裁定

### 1. P1 配额门 fail-closed — **CONFIRMED**

**代码证据**（bin/codeworld/src/main.rs）：

- L871-888：随行资产 id 逐个 `self.asset_mirror.size_of(id)`，`None` → warn 日志 + `anyhow::bail!`。**拒绝路径在 `self.sandboxes.remove(target_id)`（L903-905）之前**——旧沙箱从未离表，资产天然不丢。
- L877-884：warn 日志原文 `"[skin] 换血拒绝：资产 id 不在镜像仓，配额无法核实（fail-closed）"`，含 `fail-closed` 字样、带 entity_id/asset_id 结构化字段；bail! 传播错误给调用方（与既有超限拒绝同路径，不吞错）。
- L860-865：注释如实记录 R1 P1 缘由（按 0 记=静默刷配额洞；镜像仓启动建仓后不更新，启动后上传的 id 全走此路）。

**测试证据**（main.rs L6417-6443 `replace_code_rejects_when_asset_id_missing_from_mirror`）：

- 真锚该语义：随行 id `av_ffffffffffffffff` **故意不播种镜像仓**，配额留默认 1MB（L6420 注释明确"模块本身装得下，拒绝的唯一原因=fail-closed"）→ 断言 `is_err`。若实现退回"按 0 记放行"，replace_code 会成功、`is_err` 断言必红——测试确实抓住 P1 洞，非空转测试。
- L6437-6442：断言拒绝后旧沙箱条目仍在 + 资产清单原样（不丢）。
- 单跑实测：`2 passed; 0 failed`（连同 `replace_code_transfers_assets_to_new_sandbox`，后者已按 fail-closed 新契约补两 id 镜像仓播种，L6367/L6380-6381）。

### 2. P2 版本漂移（net.rs v2.3→v2.4）— **CONFIRMED**

- `grep v2\.3` 于 bin/codeworld/src/net.rs：**零命中**。
- `grep v2\.4`：4 处（L175 字段注释、L762/766/787 测试注释）。

### 3. P2 口径对齐（两源账 + 真无状态校验器）— **CONFIRMED**

**五处口径逐一核对，全部一致**（"配额账两源（引擎+皮）；魂走 64MB 内存上限独立账，两账并行"）：

| 处 | 位置 | 原文锚 |
|---|---|---|
| ① | crates/arena/src/assets.rs 模块头 | L6-9：`配额账两源：wasm 字节（引擎）+ 资产段（皮…）；魂（线性内存）由 64MB static_memory 独立封顶——三段合计封顶的总精神由两本账共同实现` |
| ② | docs/api.md 五点五配额行 | L429：`配额账两源=引擎（模块字节）+皮（资产）…魂（线性内存）走 64MB 内存上限独立账——两账并行` |
| ③ | docs/api.md 五点五直觉锚 | L434：`配额管"盒子里装多少东西"，64MB 内存上限管"魂的房间多大"——两本账并行不混` |
| ④ | docs/game-menu.md 1.7 总账行 | L109：`引擎+皮入配额账；魂走 64MB 内存上限独立账，两账并行。超限拒绝装戴/换血，资产不丢` |
| ⑤ | bin/codeworld/src/config.rs 注释 | L92-95：`配额账两源=引擎+皮；魂走 64MB 内存上限独立账，两账并行` |

**真无状态校验器**（assets.rs）：

- L27-31：`SandboxAssetQuota` 仅剩 `limit` 字段——`module_bytes`/`assets_bytes` 落账字段与 getter 已删净（全文 grep 无其他字段/getter）。
- L55-57（admit）与 L63-69（admit_swap）均委托共用核心 `check`（L72-86），按当次实况判定、不落账。
- L52-54：`admit` 文档注记到位（见第 4 项）。

### 4. P2 注释写实（admit 留给装戴刀）— **CONFIRMED**

- assets.rs L52-54：`注：当前生产路径仅用 [`Self::admit_swap`]（换血门）；本方法留给装戴刀（出生装戴时校验），两方法同口径同边界（admit_and_admit_swap_same_ semantics 测试锚定）`。
- 与现实一致：bin 侧唯一生产消费点是 `admit_swap`（main.rs L890-891），admit 无 runtime 消费点（与 cc-report §4.4 边界声明吻合）。

---

## 二、新缺陷扫描

### 结论：**无新缺陷**（P0/P1/P2 均无 R1 返工引入或未暴露的问题）

重点面逐项核查：

| 扫描面 | 结论 | 证据 |
|---|---|---|
| admit/admit_swap 共用 check 语义漂移 | 无漂移 | 两入口同委托 `check`（assets.rs L72-86），Err 同携带 `total - limit` 差额字节；`admit_and_admit_swap_same_semantics`（L173-185）断言同输入两入口 Ok/Err 与差额字节全等（恰等/超限/空载三档）。单跑全绿 |
| mirror.rs from_registry 跳过 + size_of None 组合路径 | 自洽 | skip 条目不入 `assets` map（mirror.rs L65-72 continue）→ `size_of` 返回 None（L118-120）→ bin 侧 fail-closed 拒绝。链路闭环；`from_registry_skips_unreadable_png`（L290-299）断言 skip 后 `size_of==None`。模块头与 skip 日志均注明 fail-closed 后果（L9-10、L69） |
| main.rs 拒绝路径日志与错误传播 | 不吞错 | 两处拒绝均 `tracing::warn!`（带结构化字段）+ `anyhow::bail!` 向上传播，与既有"超限拒绝""payload 非法还原旧沙箱"同模式；bail 前的 fuel 读取（L854-859）只读无副作用 |
| index.html skinCache failed 恒兜底 | 无死循环无泄漏 | `ensureSkin`（L415-423）：`skinCache.has(id)` 即 return——failed 后**永不再发请求**；`img.onerror` 只置 `state='failed'`，无重试定时器/循环。绘制层（L1160-1161）`state!=='ready' || !naturalWidth` → continue 走既有默认圆+glyph。Map 容量上限=出现过的唯一 asset_id 数（受资产目录规模约束），单 entry 一个 Image 对象，无泄漏放大 |
| Entity.assets serde 兼容 | 兼容且测试绿 | entity.rs L174-175：`#[serde(default)] pub assets: Vec<String>`（与 bound_to L121-122 同款先例）；`entity_assets_serde_backcompat_old_snapshot` + `entity_assets_serde_roundtrip` 单跑 `2 passed` |

---

## 三、机器验证（亲跑原文）

### 1. `cargo test --workspace`（经 cc-cargo.mjs 转发器）

**1063 passed / 0 failed / 2 ignored**，分 crate 原文：

```
bin codeworld:  test result: ok. 446 passed; 0 failed; 2 ignored
arena:          test result: ok. 149 passed; 0 failed
core:           test result: ok. 223 passed; 0 failed
physics:        test result: ok. 85 passed; 0 failed
render:         test result: ok. 72 passed; 0 failed
space:          test result: ok. 30 passed; 0 failed
storage:        test result: ok. 26 passed; 0 failed
time:           test result: ok. 16 passed; 0 failed
spike (A/B/C):  6 + 5 + 5 = 16 passed; 0 failed
```

对账：R1 纯净树 1030（§8）→ +兄弟 D-001b 桥卡 31 = 1061（§7）→ +D-012 收尾 2 = 1063，与最新 commit b1b0e3b 信息一致，账目自洽。

### 2. `cargo clippy --workspace -- -D warnings`（门禁档，与 R1 同口径）

```
Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.35s
```

**0 错误 0 警告**（增量缓存命中直接 Finished，无任何 error/warning 输出；-D warnings 下任何 lint 会以 error 输出并致非零退出）。

补充 `--all-targets` 档复核：**5 处测试代码 lint，全部存量**（非 R1 返工引入）——归属核查（`git show b3be411`/`198fbed` 实证早于或平行于本卡）：

- crates/physics/src/default.rs:1679 `type_complexity` — D-012 卡（R1 §6.1 已记录归该卡）
- crates/core/src/mask.rs:448 `manual_range_contains` — D-017 卡（R1 §6.1 已记录）
- bin/codeworld/src/avatars.rs:501 `manual_range_contains` — b3be411 基线已存在（D-017 时代 PNG 构造助手复制三处之一）
- bin/codeworld/src/net.rs:1181 `manual_range_contains` — 同上，b3be411 基线 L1175 已存在
- bin/codeworld/src/main.rs:7657 `unnecessary_into_owned` — b3be411 基线 L7371 已存在

**门禁档不受影响**（`--workspace` 不含 `#[cfg(test)]` 代码）。此 5 处为存量欠账，建议归 D-012/D-017 卡或独立快赢刀，不算本卡缺陷。

### 3. 提交链与漂移归属

- `git log --oneline -6`：`198fbed fix(core,bin): D-051a R1 返工` 在链上（b1b0e3b → 82e0846 → ef133ec → **198fbed** → b3be411 → 3721e69）。
- `git diff 198fbed..HEAD`：
  - `crates/arena/src/assets.rs`、`bin/codeworld/src/mirror.rs`：**零漂移**。
  - `bin/codeworld/src/main.rs` +687/−3：兄弟卡改动（ef133ec D-001b 外桥 + 82e0846 D-012 收尾）。过滤 `skin|asset|quota|fail-closed|mirror` 关键词 diff **零命中**——D-051a 相关代码未被触碰，本卡提交树内的实现与当前 HEAD 完全一致。
  - `crates/aria/src/assets.rs`（任务原文笔误路径）：不存在该文件，正确路径为 `crates/arena/src/assets.rs`（已按更正后路径核查）。

---

## 四、总结论

**R2-QUALITY: PASS**

四项必改全 CONFIRMED（fail-closed 门真锚语义、版本清零、五处口径一致+真无状态校验器、admit 注记写实）；无新缺陷；门禁测试 1063 全绿、clippy 0 错误；本卡文件 198fbed 后零漂移，兄弟卡改动未波及。
