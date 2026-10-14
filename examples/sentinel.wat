;; 哨塔（sentinel）：超视距情报的最简实现（decisions.md #018 哨兵网络正解之一）。
;; 生命恒真（自治锚合同：存续由自己代码决定，与创造者在线无关）。
;; 每拍看一眼视野：有人过境就发观察信号 log(2)——marker 2 = 哨兵观察。
;; 部署方式：玩家进场代码用 env.spawn 把本文件全文作为造物代码提交，
;; 出生点限创造者 ±10 格（docs/api.md spawn 条目）。更聪明的哨塔（记方位、
;; 组网回报、伪装）留给你自己写。
(module
  (import "env" "visible_count" (func $visible_count (result i32)))
  (import "env" "log" (func $log (param i32)))
  (func (export "life") (result i32) i32.const 1)
  (func (export "on_tick")
    (if (i32.gt_s (call $visible_count) (i32.const 0))
      (then (i32.const 2) (call $log)))))
