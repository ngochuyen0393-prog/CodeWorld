;; 飞剑（D-020a 技能栏槽位 2 模板）：出生初速由服务端定装（斜射 6/3），
;; 命中=接触通道代码攻，带击退力 2（attack_at 第 5 参——力度=攻击者声明制）。
(module
  (import "env" "attack_at" (func $attack (param f64 f64 i32 i32 f64) (result i32)))
  (memory (export "memory") 1)
  (func (export "life") (result i32) (i32.const 1))
  ;; 接触即斩：空 payload 刀 + 击退 2（打中的目标下拍被推开）
  (func (export "on_contact") (param f64 f64)
    (drop (call $attack
      (local.get 0) (local.get 1)
      (i32.const 0) (i32.const 0)
      (f64.const 2.0)))))
