;; 箭塔（新手模板 4：站桩哨塔——scan 找最近敌人 + 贴身出刀 + on_attack 盾拦）
;; 性格：钉在原地不走动（塔的尊严）。视野内最近的活物贴身（<=1.5 格）才出刀；
;;       有人砍你：盾还有 3 点时拦截（marker 11=拦下，盾-1），盾尽放行（marker 12）。
;; 你改得出的第一个进化：把盾初值 3 改大；或把出刀距离 1.5 改大试试够不够得着——
;;       世界说够不着就是够不着（0.5 接触容差是物理判距，不是你的射程）。
;; 进阶作业：够不着的敌人塔不追（追人不是塔的本分）——自己 spawn 一发射弹
;;       （出生初速 + on_contact 代码攻，见 docs/newbie-first-thrower.md），
;;       远程塔就是你的第一件毕业作品。
(module
  (import "env" "log" (func $log (param i32)))
  (import "env" "get_pos" (func $get_pos (result f64 f64)))
  (import "env" "scan" (func $scan (param i32 i32) (result i32)))
  (import "env" "attack_at" (func $attack_at (param f64 f64 i32 i32 f64) (result i32)))
  (memory (export "memory") 1)
  (global $shield (mut i32) (i32.const 3))
  (func (export "life") (result i32) i32.const 1)
  ;; 盾拦（on_attack 拦截位）：盾有余 -> 拦下这一刀（marker 11，盾-1）；
  ;; 盾尽 -> 放行（marker 12）。返回 1=拦截，0=不拦（挨这一刀）。
  (func (export "on_attack") (param $payload i32) (result i32)
    (if (i32.gt_s (global.get $shield) (i32.const 0))
      (then
        (global.set $shield (i32.sub (global.get $shield) (i32.const 1)))
        (i32.const 11) (call $log)
        (return (i32.const 1))))
    (i32.const 12) (call $log)
    (i32.const 0))
  ;; 主循环：站桩扫描。没有目标 -> 待机（marker 0）；贴身 -> 出刀（marker 2）；
  ;; 够不着 -> 干瞪眼（marker 1——塔不走动，想够得远去写你的远程塔作业）
  (func (export "on_tick")
    (local $mx f64) (local $my f64) (local $ex f64) (local $ey f64)
    (local $dx f64) (local $dy f64) (local $dist f64)
    ;; 1. 扫描最近敌人；视野里没有就待机（marker 0）
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
    ;; 5. 够不着：干瞪眼（marker 1，塔不走动）
    (i32.const 1) (call $log)))
