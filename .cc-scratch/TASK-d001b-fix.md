# TASK: D-001b 收尾小刀——跨模型双审 P1×2 + 顺手 P2 清偿

你是 CodeWorld 项目施工者。D-001b 外桥最小实测（commit ef133ec）已过独立双审（R1 代码质量审 PASS-WITH-NOTES：0×P0/1×P1/7×P2；R2 需求匹配审 PASS-WITH-NOTES：1×P1/4×P2）。本刀把两件 P1 修掉 + 顺手清高价值 P2，然后全绿前进式 commit。

## 铁律
- 禁新增依赖（Cargo.toml 零改动）；禁 amend/reset/rebase（共享树前进式 commit）。
- 不碰 .cc-bugs/.cc-context/.cc-progress（兄弟会话在途）；不 add 兄弟的 WIP 文件。
- 物理常量/现有判序语义以代码现实现为准；本刀不改任何已测试的对外语义（除 P1-B 明确的文档口径）。

## P1-A（必修）：connect 阶段不受 5s 超时护栏约束
`bin/codeworld/src/bridges.rs` http_post（约 :393-396）：`TcpStream::connect` 无超时参数，`set_read/write_timeout` 不作用于 connect——对静默丢 SYN 的端口，connect 走 OS 默认（Windows ~21s），单工作线程队头阻塞远超 5s 护栏承诺。
修法：vault 已限定 host ∈ {127.0.0.1, localhost}，解析成 SocketAddr 后用 `TcpStream::connect_timeout(&addr, timeout)`；localhost 直接映射 127.0.0.1（不走 DNS 悬挂）。加单测覆盖「host→SocketAddr 解析」helper（含 localhost 大小写形态，见 P2-D）。

## P1-B（必修）：死亡竞态条款文档-代码对齐
实现语义（已测试锚定 `collect_bridge_sends_dead_entity_drops_letter`，且与任务书日志串逐字对齐，**代码不动**）：信件在世界**出站时点**仍需发件人存活——出站前死亡（含同拍自尽）→信作废不送（INFO）；已出站在途→照飞；回信抵达时人已死→丢弃+INFO。
`docs/api.md` bridge_send 词条（约 :364）"你死前发的信照发，回信你收不到" 措辞与上冲突 → 改写为精确版：「信件在**世界出站时点**仍需你存活：出站前死亡（含同拍）→ 信作废不送；已出站的信照飞不撤；回信抵达时你已死 → 回信丢弃。」`docs/design-bridge.md` §2.3 镜像条款如有同类措辞一并对齐（只改措辞不改语义）。

## P2 顺手清（每件都小，做完即绿）
- P2-A sandbox.rs `call_on_bridge_reply`（约 :1965-1993）：签名核验（get_typed_func）**前移到 grow+写回信之前**——错误签名导出时不再留下"字节已落内存但回调没跑"的半副作用。测试保持绿（如断言依赖顺序则同步改）。
- P2-B bridges.rs vault `load()`（约 :75-107）：read_to_string 瞬时失败（杀软/备份锁）时不更新缓存 mtime（保留旧值下拍重试），防"空表+新 mtime"把桥卡死到下次手改文件。加单测：写坏文件（无读权限形态可用目录替代法或注入失败——用可测的最小形态）。
- P2-C bridges.rs worker_loop（约 :353-387）：每封信处理包 `catch_unwind`，panic → warn 日志（含线程存活状态可见性），不静默死线程。envelope expect 保持（不可达失败）。
- P2-D bridges.rs host 比较大小写不敏感（约 :137）：`http://LocalHost:8080` 应被接受（等价 127.0.0.1）。加单测。
- P2-E config.rs：`bridge_timeout_s=0` 按既有"非法回退默认"惯例拒收（0 → 默认 5），加单测。
- P2-F 文档三处：①`docs/design-bridge.md` §2.1 信封示例更新为实现形态（entity 十进制字符串 + payload JSON 字节数组，api.md 已是对的）；②§2.5 护栏表补 `REPLY_READ_CAP=1MB`（回信读保险丝，const 非 config，理由一句话）+ `docs/game-menu.md` 1.7 总账同行；③bridges.rs 模块头 + api.md bridge_send 词条各加一句「客户端局限：不支持 Transfer-Encoding: chunked（MVP，回信需 Content-Length/短连接 EOF）」。
- P2-G api.md 出站日志口径：日志字段含 url（保险库内容非信件内容，单机无泄露面）——api.md 措辞对齐为「日志只记 entity/port/seq/字节数/端点 url，信内容零入日志」。

## 硬 Gate（全过才算完）
1. `cargo test --workspace` 全绿（当前基线 1063 passed / 0 failed，本刀只增不减）。
2. `cargo clippy --workspace` 0 警告 0 错误（把输出存 `.cc-scratch/d001b-fix-clippy.log` 留证）。
3. `cargo test -p codeworld --bin codeworld -- --ignored` 2 passed（含 bridge_e2e 五步链）。
4. 新增/修改测试全部行为级断言（禁"编译通过即过"）。
5. 测试输出存 `.cc-scratch/d001b-fix-test.log`。
6. 前进式 commit（一个 commit 收全部），消息格式 `fix(arena,bin,docs): D-001b 双审收尾——connect_timeout+死亡条款对齐+P2×7`。

## 已知信息（不需要查，直接用）
- cargo 不在 CC 沙箱 PATH：先 `export PATH="$HOME/.cargo/bin:$PATH"`；若仍丢用 node 转发器 `$LOCALAPPDATA/Temp/cc-cargo.mjs`（参数原样转发）。
- 行号基于 ef133ec，工作树 HEAD 已前进（b1b0e3b），以 grep 实况为准。
- 本机防火墙把 connect-refused 静默吞成超时（实测）——connect 超时测试别依赖 refused 即回。
- 受影响文件清单：crates/arena/src/sandbox.rs、bin/codeworld/src/bridges.rs、bin/codeworld/src/config.rs、bin/codeworld/src/main.rs（可能零改动）、docs/api.md、docs/design-bridge.md、docs/game-menu.md。
- 双审报告全文：`$LOCALAPPDATA/Temp/cc-review-r1-result.json` 与 `cc-review-r2-result.json` 的 result 字段（如需原文细节可读）。

## 完成后
报告写 `D:/projects/CodeWorld/.cc-scratch/cc-report-d001b-fix.md`（改动清单/Gate 结果/测试计数增减/遗留）。然后直接 commit，不要等确认。
