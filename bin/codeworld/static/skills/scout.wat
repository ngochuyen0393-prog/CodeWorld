;; 侦察弹（D-020a 技能栏槽位 4 模板）：出生初速由服务端定装（向东 3 格/拍），
;; 每拍 log 心跳递增 marker——控制台 player_log 事件流可见（白字）。
(module
  (import "env" "log" (func $log (param i32)))
  (memory (export "memory") 1)
  (global $n (mut i32) (i32.const 0))
  (func (export "life") (result i32) (i32.const 1))
  (func (export "on_tick")
    (global.set $n (i32.add (global.get $n) (i32.const 1)))
    (global.get $n) (call $log)))
