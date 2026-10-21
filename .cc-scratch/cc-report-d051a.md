# cc-report-d051a — D-051a 皮入沙箱工程收工报告

> 卡：t_6d70ed33（D-051a，priority 70）· 宪法级新机制 · 2026-10-22 凌晨收工
> 基线：dc74819（D-012 已定版提交后的干净树）· Gate：cargo test --workspace 1029 全绿 + clippy -D warnings 零警告

## 0. 一句话

沙箱三段论落地：代码（引擎）/数据（魂）/**皮（资产段）**同箱——装戴清单入实体与沙箱（双处镜像）、换血随行（夺沙箱=夺全套）、配额门（拒绝换血资产不丢）、AssetMirror sha256 去重（同图全局一份）、快照 v2.4 asset_ids 恒有字段+前端取图+替身兜底。

## 1. 改动文件清单（15 文件，逐文件一句）

**已提交（三笔前进式 commit：cbf609c → 2952db7 → 3721e69；R1 返工追加 198fbed，见 §7）**

| 文件 | 改动 |
|---|---|
| `crates/arena/src/assets.rs`（新） | SandboxAssetQuota 配额账：admit 装戴（超限整次拒绝不裁剪、失败不落账）+ admit_swap 换血（只校验不落账，Err 携带超额字节）；SANDBOX_ASSET_QUOTA_BYTES=1MB；8 测试（R1 重构为真无状态校验器，见 §7） |
| `crates/arena/src/lib.rs` | +1 行 `pub mod assets;` |
| `crates/arena/src/sandbox.rs` | SandboxData.assets 字段（装戴清单，数据非执行注释）+ SandboxManager::replace_sandbox_assets（移动语义转移）+ replace_sandbox Ok 分支接线（拆旧前转移，失败路径资产随旧沙箱带回）；4 测试 |
| `crates/core/src/entity.rs` | Entity.assets 字段（serde default 空，旧快照兼容；radius/mass 先例）+ 构造器初始化；3 测试 |
| `bin/codeworld/src/mirror.rs`（新） | AssetMirror 镜像仓：asset_id→元数据 + sha256→首 id 去重账；from_registry 启动建仓（PNG 不可读跳过）、size_of（配额记账数据源）、unique_contents（同图一份判据）；6 测试 |
| `bin/codeworld/src/config.rs` | asset_quota 字段（默认 arena 同源常数）+ CODEWORLD_ASSET_QUOTA 覆盖（0/非法回退默认——0 会拒绝一切换血，无"禁皮"语义）；3 测试 |
| `bin/codeworld/src/main.rs` | EntitySandboxMap 加 asset_mirror/asset_quota 字段+装配；replace_code 换血配额门（校验在 remove 之前=旧沙箱从未离开映射表，资产天然不丢；R1 起 id 查不到镜像仓 fail-closed 拒绝）+ `[skin] 资产随行` INFO（非空才打）；remove() 带皮销毁 INFO 日志；main() 从登记表建仓；R1 后 4 测试+播种助手 |
| `bin/codeworld/src/net.rs` | EntitySnapshot.asset_ids（恒有字段空数组，同 attacks/fields 先例）+ build_snapshot 投影 e.assets；2 测试 |
| `bin/codeworld/static/index.html` | skinCache Map（id→{img,state}）+ ensureSkin（首见拉图 `/assets/avatars/{id}.png`，failed 恒兜底不再重试）+ 快照到达触发取图 + 绘制层按 radius 等比贴图（像素风关平滑，未加载/失败回落既有默认圆+glyph=VRChat 替身兜底零新代码路径） |
| `Cargo.toml` / `Cargo.lock` / `bin/codeworld/Cargo.toml` | workspace+bin 加 sha2 0.10（指纹去重判据） |
| `docs/api.md` | 「五点五、资产段」小节：沙箱三段论四语义表（装戴/随行/配额/改皮战术）+ 直觉锚 + decisions/net-protocol 交叉引用（R1 配额行改两源口径） |
| `docs/net-protocol.md` | **v2.4**（见下"版本顺延"）：§3 实体字段 asset_ids 恒有行、§8 版本记录、§9.4 快照指纹+前端取图+替身兜底 |
| `docs/game-menu.md` | 1.7 总账 +1 行：单沙箱资产配额 1MB ⚠️ 待土豆拍板（R1 行文改两账并行口径） |
| `docs/progress.md` | 现在 节 D-051a 收工更新（含实测 1029 计数） |
| `docs/decisions.md` | **不改**（#051 已入宪，本刀纯工程落地，卡面 §4.5） |

## 2. 测试结果

- **Gate（首刀）**：`cargo test --workspace` **1029 passed / 0 failed**（基线 1000 → **+29**，各 crate：arena 125→137、core 220→223、bin 410→424，其余零回归）；`cargo clippy --workspace -- -D warnings` 零警告。
- **新增测试 29 个全列**：
  - arena assets（8）：default_limit_is_one_megabyte / admit_under_limit_ok（R1 前名 admit_under_limit_records_account）/ admit_exactly_at_limit_passes / admit_over_limit_rejects_with_diff（R1 前名 …_and_keeps_account）/ admit_swap_under_limit_ok / admit_swap_over_limit_rejects_with_diff / admit_and_admit_swap_same_semantics（R1 替换 admit_swap_is_side_effect_free）/ admit_zero_assets_and_zero_module_ok
  - arena sandbox（4）：replace_sandbox_transfers_assets_to_new_sandbox / replace_sandbox_failure_keeps_assets_in_old_sandbox / replaced_sandbox_assets_writable_by_new_code（卡面 5 改皮写口存在性）/ replace_sandbox_assets_empties_old_sandbox
  - core entity（3）：entity_assets_serde_roundtrip / entity_assets_serde_backcompat_old_snapshot / new_entity_starts_with_empty_assets
  - bin mirror（6）：sha256_hex_is_stable_and_hex / insert_records_size_and_sha_by_id / size_of_unknown_id_is_none / from_registry_dedups_same_content_to_one_copy / from_registry_counts_distinct_contents_separately / from_registry_skips_unreadable_png
  - bin config（3）：default_asset_quota_matches_arena_constant / load_asset_quota_env_overrides_default / load_asset_quota_invalid_or_zero_falls_back_to_default
  - bin main（3+1）：replace_code_rejects_when_payload_plus_assets_exceed_quota / replace_code_transfers_assets_to_new_sandbox / sandbox_removal_drops_assets_with_sandbox / replace_code_rejects_when_asset_id_missing_from_mirror（R1 新增）
  - bin net（2）：snapshot_entity_carries_asset_ids / snapshot_asset_ids_empty_array_always_present
- **卡面负面路径 4 条全锚**：①配额超限拒绝装戴（admit_over_limit…，Err 携带超额字节）②换血挤不下拒绝换血不丢资产（arena 层 replace_sandbox_failure_keeps_assets + bin 端到端 replace_code_rejects_when_payload_plus_assets_exceed_quota——拒绝后旧沙箱仍在、清单原样）③指纹去重同图一份（from_registry_dedups…：两 id 同图 → unique_contents==1 且双双可查大小）④旧快照无 assets 字段反序列化回空（entity_assets_serde_backcompat_old_snapshot）；R1 追加⑤id 不在镜像仓 fail-closed 拒绝。
- **TDD 红绿全程**：assets.rs/mirror.rs 先失败桩看 RED 再实现；sandbox/entity/main/net 为缺字段缺方法编译错 RED 后实现；R1 fail-closed 先退回旧 0 记口径看红（测试抓住 P1 洞）再实现看绿。修正一处自违规：assets.rs 与 mirror.rs 初稿实现+测试同笔写入，按铁律退桩重走红绿。
- 勘误：commit 2952db7 信息写"15 新测试"，bin 实际 14（6+3+3+2）——commit 已落不 amend（共享树立规），以本报告为准。

## 3. 冒烟证据（卡面三场景）

1. **换血随行**：arena 原语层 `replace_sandbox_transfers_assets_to_new_sandbox`（换血后新沙箱读得到完整清单）+ `replace_sandbox_assets_empties_old_sandbox`（移动非复制，旧沙箱清空防双重持有）+ bin 端到端 `replace_code_transfers_assets_to_new_sandbox`（走 map.replace_code 全链路：配额门→replace_sandbox→随行→life 检查）。日志锚：`[skin] 资产随行 entity=? count=? ids=[...]`（非空才打）与 `[skin] 资产随沙箱消亡`（remove 带皮时）。
2. **指纹去重**：`from_registry_dedups_same_content_to_one_copy`——同 PNG 内容上传两次（两 asset_id），镜像仓 id 层全量可查（size_of 双双命中且相等）而 unique_contents()==1；异内容对照组 from_registry_counts_distinct_contents_separately 锚"去重不是合并"。
3. **替身兜底**：前端仅当 `state==='ready' && naturalWidth>0` 才 drawImage；loading/failed/清单缺 id 一律走既有默认身体圆+glyph 路径（该路径零改动=兜底是存量行为）；ensureSkin onerror 永久置 failed 本会话不重试。Rust 侧无此测试面（纯前端视觉），内联脚本语法检查过（2952db7 注记），**浏览器视觉验收留土豆**（环境权限边界：沙箱无浏览器）。

## 4. 需土豆知道的四件事

1. **版本顺延**：卡面写"快照 v2.2→v2.3"，但 v2.3 已被 D-012 力场占用（卡面写作时 D-012 未定版）——本刀实取 **v2.4**，net-protocol §8 版本记录内已注明缘由。
2. **并发会话共卡**：本卡由两个会话并行执行（shared tree）。撞车四处均已收敛：mirror.rs 实现（对方 GREEN 与我设计同构，已验全绿）、config.rs（对方全做，照收）、net.rs 同名测试对撞+helperworld 笔误（对方自修，终态保留单一版本）、api.md 双份资产段小节（去重保留单节）。终态全绿已逐文件对账 diff 零残渣；提交三连（cbf609c/2952db7/3721e69）内容经双方核验。
3. **配额 1MB 待拍板**：SANDBOX_ASSET_QUOTA_BYTES 临时值，CODEWORLD_ASSET_QUOTA 可调（0=非法回退默认——0 会拒绝一切换血，无"禁皮"语义）；game-menu 1.7 已标 ⚠️。
4. **本刀最小边界（卡面 §1.C）**：出生装戴只挂字段未接 spawn 参数（归挂实体刀/参数化后续刀）；SandboxAssetQuota.admit 尚无 runtime 消费点（装戴刀接入），admit_swap 已在 replace_code 实装。env.spawn 未动、零新 import（改皮=直接改资产段，写口存在性有测试锚）。

## 5. 验收建议（人工三分钟）

跑 `start.bat` + 浏览器：① `POST /upload/avatar` 传两张同图 → 日志第二条 `[mirror] 同指纹去重`+建仓行 `unique_contents=1`；② 对带皮实体出刀换血 → 日志 `[skin] 资产随行`；③ 浏览器看实体贴图渲染，改坏一张 png 后重启 → 前端回落圆形不白屏；④ R1 后给实体挂一个盘上不存在的 asset_id 再换血 → `[skin] 换血拒绝：资产 id 不在镜像仓（fail-closed）` 且旧皮还在。

## 6. 补充对账（重续会话复核，2026-10-22）

由重续会话在收尾阶段独立复核，与上文零冲突，补三点：

1. **Gate 复跑确认**：收尾全量重跑 `cargo test --workspace` = **1029 passed / 0 failed**；卡面原文 clippy 命令 0 错误。补充：`--all-targets` 档位会额外暴露 2 个**兄弟卡**测试代码 lint（crates/physics D-012 `type_complexity`、crates/core/src/mask.rs:448 D-017 `manual_range_contains`），非本卡文件未动，归各卡修。
2. **撞车收敛补记**：除上文四处外，前端 `disposeTag` 曾被并发编辑截断成坏断言（`wait_left !== changed`），重续会话已按原样还原并跑内联脚本语法检查过；net.rs 我侧曾误造重复测试块+`helperworld()` 笔误，已去重修复，终态单一版本（grep 对账零重复定义）。
3. **提交内容核验**：三笔 commit 与工作树一致（git diff 干净），progress.md 的 1029 计数与实测一致；本报告即两会话合并后的最终版，以 `.cc-scratch/cc-report-d051a.md` 副本为验收凭据。

## 7. R1 返工（2026-10-22，commit `198fbed`）

按 R1 卡四项全落地，前进式一笔：

### 改动

1. **P1 配额门 fail-closed**（main.rs replace_code）：随行资产 id 查不到镜像仓 → **拒绝换血**（原按 0 记放行=静默刷配额洞：镜像仓启动建仓后不更新，启动后上传的资产 id 全走 0 记路）。warn 日志 `[skin] 换血拒绝：资产 id 不在镜像仓，配额无法核实（fail-closed）`；与超限拒绝同路径（校验在 remove 之前，旧沙箱原样保留资产不丢）。同步口径：mirror.rs 模块头/from_registry 跳过日志/size_of_unknown 测试注释。
2. **P2 版本漂移**：net.rs 四处注释 v2.3→v2.4（grep 复核 v2.3 清零）。
3. **P2 口径对齐（两源）**：assets.rs 模块头改"配额账两源（引擎+皮）；魂走 64MB 内存上限独立账"（api.md 五点五同口径交叉引用）+ game-menu 1.7 行 + api.md 五点五配额行 + config.rs 注释，五处统一。**SandboxAssetQuota 落写真无状态校验器**：删除 module_bytes/assets_bytes 落账字段与 getter（文档说无状态就真无状态），admit/admit_swap 共用 check 核心。
4. **P2 注释写实**：admit 文档注记"当前生产路径仅用 admit_swap（换血门）；admit 留给装戴刀"。

### 测试（TDD 红绿）

- 新增 `replace_code_rejects_when_asset_id_missing_from_mirror`（bin）：沙箱带镜像仓没有的 id → 拒绝且旧沙箱 assets 原样。红绿可追溯：先临时退回旧 0 记口径看**红**（测试抓住 P1 洞）再实现看绿。
- `replace_code_transfers_assets_to_new_sandbox` 随 fail-closed 新契约补镜像仓播种（两 id 各 100B）。
- assets.rs 保持 8 测试：原 side-effect-free 测试（无状态化后空泛）替换为 `admit_and_admit_swap_same_semantics`（两入口同口径同边界锚：Ok/Err 与差额字节全等）。
- Gate：`cargo test --workspace` **1061 全绿 0 失败**（1029 + 本卡 1 + 兄弟 D-001b 桥卡在途测试 31）；clippy 本卡 hunks 零 lint（workspace 4 错全在桥卡在途代码 bridges.rs/桥循环，归该卡）。

### 不改（照卡记录在案）

mirror insert 同 id 异内容防御缺口（调用面不可达，装戴刀再议）；配额按 id/sha 份、空 payload 过门不对称（玩法语义留土豆终审）；commit 计数漂移（禁 amend，§2 已勘误）。

### 提交方式（共享树选择性提交，progress.md L69 先例）

R1 期间 D-001b 外桥卡在途未提交（bridges.rs 未跟踪 + config/main/sandbox/api/game-menu 混合 hunks）。本卡选择性提交：内容标记过滤提取本卡 hunks（awk/node 按 fail-closed 等标记选块；config/sandbox/game-menu 三处 hunk 内交错用 `-U0` 拆分，menu 补丁再手工剥离兄弟外桥行）→ 真 index `git apply --cached`（暂存后全量复核：恰 8 文件、桥符号仅剩既有 probe_bridge 上下文行）→ `git commit -F`。工作树兄弟改动零触碰（提交后 status 复核其 hunks 完整留存）。**commit：`198fbed`（8 文件，+126/−118）**。

## 8. R1 提交树隔离复验（worktree 实证）

`git worktree add` 检出 `198fbed` 纯净提交树（不含兄弟桥卡任何在途内容）全量复验：

- `cargo test --workspace`：**1030 passed / 0 failed**（bin 425 / arena 137 / core 223 / physics 85 / render 72 / space 30 / storage 26 / time 16 / spike 16——与工作树 1061 的差=兄弟在途 31 测试，恰好对账）
- `cargo clippy --workspace -- -D warnings`：**0 错误**（反证工作树 clippy 的 4 错全部属于 D-001b 桥卡在途代码，与本卡无关）
- 复验后 worktree 已清理（git worktree remove --force）

By CC.

## 9. R2 确认双审（2026-10-22，宪法级收尾刀）

R1 返工落地后由独立双镜头确认审（跨模型：质量=sonnet、需求=opus，均只读封写）：

- **质量审 R2-QUALITY: PASS**——R1 四项必改全 CONFIRMED（fail-closed 门真锚语义/版本 v2.3 清零/五处两源口径一致+真无状态校验器/admit 注记写实）；无新缺陷；门禁 1063 全绿 clippy 0 错；198fbed 后本卡文件零漂移（兄弟 D-001b/D-012 改动未波及，git diff 关键词过滤零命中）。报告：`$LOCALAPPDATA/Temp/cc-d051a-r2-quality.md`。
- **需求审 R2-REQ: PASS**——卡面五点全 SATISFIED（资产段双处镜像+数据非执行零燃料路径/配额 1MB 常数+总账+整次拒绝/夺取无特殊分支+失败原样保留/快照恒有字段+前端兜底+sha256 去重/改皮写口测试锚）；边界三项如实未越界（spawn 未动/零新 import/decisions 未改）；四笔提交链完整；五个关键测试亲跑全绿。报告：`$LOCALAPPDATA/Temp/cc-d051a-r2-req.md`。
- 两审均亲跑 `cargo test --workspace` = 1063/0（与 Hermes 独立验证一致）。

宪法级刀完整闭环：实现 → R1 审查（P1×1+P2×3）→ 返工 198fbed → R2 双审 PASS。等土豆终审（request-review）。
