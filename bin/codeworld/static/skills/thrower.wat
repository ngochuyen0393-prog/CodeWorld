;; 投掷器弹（D-020a 技能栏槽位 1 模板）：出生初速由服务端定装（向东 8 格/拍），
;; 命中=接触通道代码攻（贴身刀打接触点，空 payload=默认击杀）。全文可抄：
;; 玩家代码等价物=自己调 spawn（出生初速第 11/12 参 + on_contact 攻）。
(module
  (import "env" "attack_at" (func $attack (param f64 f64 i32 i32 f64) (result i32)))
  (memory (export "memory") 1)
  (func (export "life") (result i32) (i32.const 1))
  ;; 接触即打：对接触点出空 payload 刀（世界点命中制结算；打不中寿命 10 拍自己收场）
  (func (export "on_contact") (param f64 f64)
    (drop (call $attack
      (local.get 0) (local.get 1)
      (i32.const 0) (i32.const 0)
      (f64.const 0.0)))))
