;; 吃草彷徨者（新手模板 2：扫描 + 走位——世界里最本源的看与动）
;; 性格：胆小的食草动物。看得见的最靠近者一旦进入 6 格就转身逃跑，
;;       拉开到 8 格开外才安心继续散步（向东溜达）。
;; 你改得出的第一个进化：把 6.0 改成 3.0（更胆小）或 99.0（永远在逃）。
(module
  (import "env" "log" (func $log (param i32)))
  (import "env" "get_pos" (func $get_pos (result f64 f64)))
  (import "env" "scan" (func $scan (param i32 i32) (result i32)))
  (import "env" "move_by" (func $move_by (param f64 f64) (result i32)))
  (memory (export "memory") 1)
  (func (export "life") (result i32) i32.const 1)
  (func (export "on_tick")
    (local $mx f64) (local $my f64) (local $ex f64) (local $ey f64)
    (local $dx f64) (local $dy f64) (local $dist f64)
    ;; 1. 看一眼四周；视野里没有别人 -> 散步（向东一步），打 marker 1
    (if (i32.eqz (call $scan (i32.const 0) (i32.const 8)))
      (then
        (i32.const 1) (call $log)
        (drop (call $move_by (f64.const 1.0) (f64.const 0.0)))
        (return)))
    ;; 2. 有人：读他的坐标（scan 写入 memory）和自己的坐标
    (local.set $ex (f64.load (i32.const 0)))
    (local.set $ey (f64.load (i32.const 8)))
    (call $get_pos)
    (local.set $my)
    (local.set $mx)
    ;; 3. 算他到我的距离
    (local.set $dx (f64.sub (local.get $ex) (local.get $mx)))
    (local.set $dy (f64.sub (local.get $ey) (local.get $my)))
    (local.set $dist (f64.sqrt
      (f64.add (f64.mul (local.get $dx) (local.get $dx))
               (f64.mul (local.get $dy) (local.get $dy)))))
    ;; 4. 距离 < 6：逃跑（marker 2，朝反方向一步）
    (if (f64.lt (local.get $dist) (f64.const 6.0))
      (then
        (i32.const 2) (call $log)
        (drop (call $move_by
          (f64.div (f64.mul (local.get $dx) (f64.const -1.0)) (local.get $dist))
          (f64.div (f64.mul (local.get $dy) (f64.const -1.0)) (local.get $dist))))
        (return)))
    ;; 5. 安全距离：继续散步（marker 1）
    (i32.const 1) (call $log)
    (drop (call $move_by (f64.const 1.0) (f64.const 0.0)))))
