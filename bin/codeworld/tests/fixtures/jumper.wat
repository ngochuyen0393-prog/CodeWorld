;; ============================================================
;; 跳跳（jumper）——D-010a thrust 自主出力演示生物
;; 性格：不安分的跳蚤。每 4 拍满力跳一下（thrust(0, 7.7) = 跳 ≤3 格高锚点），
;;       其余拍省着不动（fuel 每拍重发，跳一下 ≈5% 配额，连跳不划算）。
;; 行为时间线（进场后）：
;;   tick % 4 == 0 : thrust(0, 7.7) 起跳——冲量入速度，本拍物理步起跳升空，
;;                   弧线回落（重力逐拍吃掉 vy），落地贴回半径线速度清零
;;   其余拍        : 原地站桩（ nothing，life 恒 1）
;; 机制备忘（docs/api.md thrust 词条）：
;;   - 冲量只作用于自己（力出自己身铁律），单次模长上限 7.7（超限归一化压回）
;;   - 满力一次 ≈5% fuel；fuel 每拍重发，本拍出力吃的是本拍预算
;;   - 落地速度全清零（含水平）——想平移用 move_by，thrust 管跳跃与腾空机动
;; 下线语义：删掉本文件 = 你下线（残影5拍消散）
;; ============================================================
(module
  (import "env" "thrust" (func $thrust (param f64 f64) (result i32)))

  (global $tick (mut i32) (i32.const 0))

  (func (export "life") (result i32) i32.const 1)

  (func (export "on_tick")
    (global.set $tick (i32.add (global.get $tick) (i32.const 1)))
    ;; 每 4 拍满力跳一下（返回值 drop：0=拒绝也照常过日子，下个周期再跳）
    (if (i32.eqz (i32.rem_u (global.get $tick) (i32.const 4)))
      (then
        (drop
          (call $thrust (f64.const 0.0) (f64.const 7.7)))))))
