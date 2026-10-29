;; 小猎手（新手模板 3：扫描 + 追击 + 贴身出刀——战斗的最小完整闭环）
;; 性格：专注的猎手。锁定视野内最近的活物，直线逼近，贴身（<=1.5 格）
;;       就出一记空 payload 击杀刀；没贴身就每拍朝他走一步。
;; 你改得出的第一个进化：把 1.5 改大（胳膊更长）试试够不够得着——
;;       世界说够不着就是够不着（0.5 接触容差是物理判距，不是你的射程）。
(module
  (import "env" "log" (func $log (param i32)))
  (import "env" "get_pos" (func $get_pos (result f64 f64)))
  (import "env" "scan" (func $scan (param i32 i32) (result i32)))
  (import "env" "move_by" (func $move_by (param f64 f64) (result i32)))
  (import "env" "attack_at" (func $attack_at (param f64 f64 i32 i32 f64) (result i32)))
  (memory (export "memory") 1)
  (func (export "life") (result i32) i32.const 1)
  (func (export "on_tick")
    (local $mx f64) (local $my f64) (local $ex f64) (local $ey f64)
    (local $dx f64) (local $dy f64) (local $dist f64)
    ;; 1. 扫描最近敌人；没有敌人就待机（marker 0）
    (if (i32.eqz (call $scan (i32.const 0) (i32.const 8)))
      (then
        (i32.const 0) (call $log)
        (return)))
    ;; 2. 读敌人坐标（scan 写入 memory）与自己坐标
    (local.set $ex (f64.load (i32.const 0)))
    (local.set $ey (f64.load (i32.const 8)))
    (call $get_pos)
    (local.set $my)
    (local.set $mx)
    ;; 3. 朝向向量与距离
    (local.set $dx (f64.sub (local.get $ex) (local.get $mx)))
    (local.set $dy (f64.sub (local.get $ey) (local.get $my)))
    (local.set $dist (f64.sqrt
      (f64.add (f64.mul (local.get $dx) (local.get $dx))
               (f64.mul (local.get $dy) (local.get $dy)))))
    ;; 4. 贴身（<= 1.5）：出刀（marker 2）
    (if (f64.le (local.get $dist) (f64.const 1.5))
      (then
        (i32.const 2) (call $log)
        (drop (call $attack_at (local.get $ex) (local.get $ey)
                               (i32.const 0) (i32.const 0) (f64.const 0.0)))
        (return)))
    ;; 5. 未贴身：朝他走一步（marker 3）
    (i32.const 3) (call $log)
    (drop (call $move_by
      (f64.div (local.get $dx) (local.get $dist))
      (f64.div (local.get $dy) (local.get $dist))))))
