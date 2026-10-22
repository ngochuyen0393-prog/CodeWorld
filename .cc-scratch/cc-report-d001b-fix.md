# D-001b 收尾小刀报告——双审 P1×2 + 顺手 P2 清偿

日期：2026-10-22 ｜ 基线：b1b0e3b ｜ TDD 全程 RED→GREEN，六件代码改动逐一先见红再转绿。

## 改动文件清单（7 个，逐个列）

| 文件 | 改什么 |
|---|---|
| `bin/codeworld/src/bridges.rs` | **P1-A** `resolve_host_addr` helper + `http_post` 改 `TcpStream::connect_timeout`（connect 不再走 OS 默认 ~21s 超时窗，localhost 直映射 127.0.0.1 不走 DNS）；**P2-B** `BridgeVault::load` 瞬时读失败（非 NotFound）置 mtime=None 保留重试语义；**P2-C** worker_loop 每信包 `catch_letter_panic`（panic→warn+归超时路径，inflight 不泄漏，线程不死）；**P2-D** validate host 大小写不敏感；**P2-F③** 模块头补 chunked 局限一句；新增测试 ×4 |
| `crates/arena/src/sandbox.rs` | **P2-A** `call_on_bridge_reply` 签名核验（get_typed_func）前移到 grow+写回之前——坏签名导出不再留"字节落内存回调没跑"半副作用；新增测试 ×1（REPLY_BAD_SIG_WAT） |
| `bin/codeworld/src/config.rs` | **P2-E** `CODEWORLD_BRIDGE_TIMEOUT_S=0` 拒收回退默认 5（warn 提示）；新增测试 ×1 |
| `bin/codeworld/src/main.rs` | 零逻辑改动——仅两处注释口径对齐（死亡竞态条款 collect/deliver 文档串、出站日志字段含 url） |
| `docs/api.md` | **P1-B** bridge_on_reply 死亡竞态条款改精确版；**P2-F③** bridge_send 词条补 chunked 局限；**P2-G** 出站日志口径改「entity/port/seq/字节数/端点 url，信内容零入日志」 |
| `docs/design-bridge.md` | **P1-B** §2.3 镜像条款对齐实现语义（三段式：出站前死作废/已出站照飞/回信时死丢弃，全带日志）；**P2-F①** §2.1 信封示例更新为实现形态（entity 十进制字符串 + payload JSON 字节数组）；**P2-F②** §2.5 护栏表补 `REPLY_READ_CAP=1MB` 行 |
| `docs/game-menu.md` | **P2-F②** 1.7 世界常数总账外桥护栏行补回信读保险丝 1MB 同行 |

## TDD 轨迹（每件先见红）

1. **P2-D RED**：`LocalHost 混合大小写应接受` 失败 → GREEN（eq_ignore_ascii_case）
2. **P2-B RED**：`瞬时读取失败不得缓存 mtime` 失败（目录替代法模拟，metadata 成功+读失败非 NotFound）→ GREEN（mtime=None）
3. **P1-A RED**：`resolve_host_addr` 编译期缺失 → GREEN（helper ×4 形态断言 + 非回环 None）
4. **P2-C RED**：`catch_letter_panic` 编译期缺失 → GREEN（panic 不炸调用方+on_panic 可见+正常路径原样返回）
5. **P2-A RED**：坏签名下内存 1→2 页（半副作用实锤）→ GREEN（核验前移，Err 后内存分毫不动）
6. **P2-E RED**：超时 0 直通失败 → GREEN（0 拒收回退 5）

## Gate 结果

| Gate | 结果 |
|---|---|
| 1. `cargo test --workspace` | ✅ **1069 passed / 0 failed / 2 ignored**（基线 1063 + 本刀新增 6 = 1069，只增不减；日志 `.cc-scratch/d001b-fix-test.log`） |
| 2. `cargo clippy --workspace` | ✅ 0 warning 0 error（日志 `.cc-scratch/d001b-fix-clippy.log`） |
| 3. `cargo test -p codeworld --bin codeworld -- --ignored` | ✅ 2 passed（含 bridge_e2e 五步链全通） |
| 4. 新增/修改测试行为级断言 | ✅ 全部断言用户可见行为（内存页数/解析结果/事件到达/mtime 重试语义/回退值），零"编译通过即过" |

## 新增测试计数（+6）

- bridges.rs：`vault_host_case_insensitive_accepts_localhost_variants`、`vault_transient_read_failure_keeps_mtime_uncached_for_retry`、`resolve_host_addr_maps_localhost_variants_to_loopback`、`catch_letter_panic_swallows_panic_and_reports`
- sandbox.rs：`call_on_bridge_reply_bad_signature_leaves_no_half_side_effect`
- config.rs：`load_bridge_timeout_zero_falls_back_to_default`

## 语义边界确认

- 已测试对外语义零改动（除 P1-B 明确的文档口径对齐）：worker e2e（`worker_delivers_reply_and_timeout_events`）与主程序 8 个桥编排测试全绿未动。
- P2-C panic 信归超时路径（Timeout 事件结算 inflight）——防泄漏是设计意图，非语义变更（panic 路径今日不可达，纯防御）。
- 旧措辞「死前发的信照发」残留仅存于 `.cc-scratch/` 存档 diff 与任务书本身，活文档（api/design-bridge/main.rs 注释）已全部对齐。

## 遗留

- 无本刀范围遗留。双审其余 P2（低价值项）与 design-bridge §2.8 挂起项维持原状。
