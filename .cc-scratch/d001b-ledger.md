# SDD ledger — plan: TASK-D001B（外桥最小实测，任务书内联）

基线：cargo test --workspace = 1029 passed / 0 failed / 1 ignored（.cc-scratch/d001b-baseline-test.log）
环境：cargo 经 node 转发器；共享树前进式 commit（禁 amend/reset/rebase）；任务书覆盖 worktree 默认——直接共享树施工（Ruling）。

Pre-flight（共享接口行）：
- 单元2(arena SandboxData+import) → 单元5(main 拍首 fill + a-1 collect)：字段名/返回码表以任务书施工项1为准，两边同源。
- 单元4(bridges.rs Letter/ReplyEvent) → 单元5(a-1 drain 回信)：事件枚举形状以施工项2为准（Reply{entity,port,seq,body}/Timeout{entity,seq}）。
- 冲突：无。开工。

任务分解：
- [x] U1 config 五项（RED 编译失败→GREEN 50 passed）
- [x] U2 arena 桥字段+import（RED E0609×14→GREEN 144；+7 测试）
- [x] U3 call_on_bridge_reply（RED E0599→GREEN 148；+4 测试；WAT global.set 笔误修）
- [x] U4 bridges.rs（9 测试绿；变异抽查 host 检查→红→恢复证实有效）
- [x] U5 main.rs 编排（RED E0599/E0609→GREEN；+8 测试）
- [x] U6 ignored e2e（--ignored 2 passed 含新 bridge_e2e 五步链断言）
- [x] U7 三件套（bat ASCII+CRLF 验证过）
- [x] U8 文档四件（api.md/menu/design-bridge/progress.md）
- [x] U9 Gate：workspace 1061/0（+26）/clippy 0/ignored e2e 2 passed/亲跑 bat ALL PASS/commit ef133ec
- [x] 报告 $LOCALAPPDATA/Temp/cc-report-d001b.md

补记 Ruling：
- R9 信封 entity 十进制字符串（serde_json u128 panic 实测；e2e 第一轮 worker 崩铁证）——api.md 示例同步
- R10 bat 断言锚：tracing ANSI 拆散 marker=N → strip ANSI 后断言（D-007 同款坑）
- R11 e2e 拓扑：世界常驻 → start /B 后台+ping 等待+taskkill；假服务器空闲自杀防文件锁
共享树协作：兄弟 D-051a R1（198fbed）中途落库修好其 fail-closed 测试；本刀 diff 复核纯净（644 行 main.rs 全我的）后逐文件 add。

Rulings：
- R1 超限判序：未启用→4 后立即查超限→2（读内存前拦截），再 port→1/已发→3/在途满→3。任务书判序未给 2 的位置；选"数据尺寸优先于路由有效性"——多重违规组合本身病态，可观测顺序只需确定。若错：返回码语义在病态调用上换位，正常调用无影响。
- R2 TDD watched-fail 替代：U4 测试与实现同批落盘，改用变异抽查（破坏 host 检查→测试红→恢复）证实测试有效性。成本：比标准 RED 少一次"实现缺失"失败形态验证；变异抽查覆盖等价。
- R3 空 payload 判序：len<=0/越界 -> 空 payload 照常排队（attack_at 同款）——空信也是信（#038 只送信不拆信）。
- R4 缺省导出回信 inflight 照减：沙箱零副作用与世界记账分离——记账泄漏会永久占满在途额度。若错：无。
- R5 非 2xx 归 Timeout 路径：回信=200 body（design 2.1），非 2xx=没有回信。若错：外脑用 3xx 重定向表达回信时收不到——外脑自己管 HTTP 语义。
- R6 回信超 payload_max 截断投递：诚实交付前缀+grow 页数随缩。若错：外脑回大信被截尾——它自己超规的。
- R7 共享树兄弟 WIP（并行会话正在改 replace_code fail-closed 语义，新测试+旧实现暂不一致=1 个既存红）：不碰不修不 add，Gate 口径=不新增红+收尾时复查是否已被兄弟修好。证据：该测试与配套实现在 HEAD 均不存在，全为我开工后兄弟写入工作区。
- R8 serde_json 默认按 key 排序：信封字段顺序不断言（玩家按字段名读），api.md 信封示例按协议顺序展示仅示意。

环境发现：本机防火墙把 connect-refused 静默吞成超时（python 实测）——bridges worker Timeout 测试按 1s 超时窗等满。
