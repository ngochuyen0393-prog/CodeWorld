;; ============================================================
;; field-demo——力场哨塔（D-012 力场出生声明家族示例，玩家文档）
;;
;; 这是什么：一个"进场第一拍在 +5 格立一座阻力护盾场哨塔"的玩家角色示例。
;; 塔出生时声明（spawn 第 15/16/17 参）：以塔身为中心、半径 3.0 格的圆域内
;; 重力不变、阻力被改写为世界阻力的 40 倍——飞进来的弹丸被强阻力急速减速
;;（护弹道不护血量：场改的是物理常数，不是谁的血条）。
;;
;; 部署方式：本文件放 players/ 目录即进场（或作为进场代码模板抄写）。
;;
;; 场参数速查（docs/api.md「力场（D-012）」节）：
;;   第 15 参 field_radius        域 [0.0, 25.0]，0=无场（默认）
;;   第 16 参 field_gravity_scale 域 [0.0, 10.0]，1.0=不变（默认）；生效重力=世界重力×scale
;;   第 17 参 field_drag_delta    域 [-1.0, +25.0]，0=不改写（默认）；生效阻力=世界阻力+delta（加法式）
;; 叠加规则（叠加 v2）：多场叠乘重力缩放、代数相加阻力 delta（正负互吃，
;; 对冲场是真实战术）；域钳制兜底（重力 [-98,0]、阻力 [0,25]——堆场买不到
;; 定律外胜利）。账本不认老板：所有场一律入账，无所有权标签。
;; 成型期：诞生后 3 拍（FIELD_FORMING_TICKS，config 可调）场不结算物理但已
;; 可见（前端虚线环=成型中，实线环=已成型）——给进攻方反应窗。
;; 零计费：场的成本=普通造物日常开销+实体名额，世界不向场收一分税。
;;
;; 数值：世界阻力默认 0.05，40 倍=2.0 → field_drag_delta = +1.95
;;（生效阻力 = 0.05 + 1.95 = 2.00；mass=1 的弹丸每拍速度衰减 = drag/mass
;; = 200%，方向不变模衰减钳零 → 弹丸进 圈 即被"粘"住）。
;;
;; 写码注意：WAT 的 local 声明必须在函数体开头；data 段字符串内不能有裸换行；
;; 多参数 import 调用用 locals+显式传参最稳（与 players/pioneer.wat 同款结构）。
;; ============================================================
(module
  (import "env" "log" (func $log (param i32)))
  (import "env" "get_pos" (func $get_pos (result f64 f64)))
  (import "env" "spawn" (func $spawn (param i32 i32 f64 f64 i32 i32 f64 f64 i32 i32 f64 f64 i32 i32 f64 f64 f64 f64 f64 i32 f64 f64 i32 i32) (result i32)))

  (memory (export "memory") 1)

  ;; ── 场塔代码全文（单行，以 \00 结尾）：有生命合同+on_field 询问应答的哨塔 ──
  ;; on_field(n_ptr, ids_cap, ids_ptr) -> i32 是力场询问应答（协议 v3，世界每拍
  ;; 一问）：返回 0=维持出生声明（笨场，本示例即此）；返回 k≠0=本拍阻力改写量
  ;; 临时换成 k（智能场：遍历 [ids_ptr, ids_ptr+8n) 的域内实体清单做敌我识别
  ;; 后返回差异化数值——识别步数自己烧，世界只提供通道）。未导出也合法：按
  ;; 出生声明结算。
  (data (i32.const 4096) "(module (import \"env\" \"visible_count\" (func $vc (result i32))) (import \"env\" \"log\" (func $log (param i32))) (memory (export \"memory\") 1) (func (export \"on_field\") (param i32) (param i32) (param i32) (result i32) (i32.const 0)) (func (export \"life\") (result i32) (i32.const 1)) (func (export \"on_tick\") (if (i32.gt_s (call $vc) (i32.const 0)) (then (i32.const 2) (call $log)))))\00")

  ;; ── 场塔外观数据（NUL 结尾）─────────────────────────────
  (data (i32.const 8192) "F\00")
  (data (i32.const 8256) "lime\00")
  ;; ── 造物实名（D-020b 第 24 参，NUL 结尾，≤16 字符；控制台播报/技能栏显示名）──
  (data (i32.const 8704) "护盾塔\00")

  (global $tick (mut i32) (i32.const 0))
  (global $tower_done (mut i32) (i32.const 0))

  (func (export "life") (result i32) (i32.const 1))

  ;; 内部工具：扫描 [4096..) 直到 NUL，返回场塔代码字节数
  (func $tower_len (result i32)
    (local $p i32)
    (local.set $p (i32.const 4096))
    (block $done
      (loop $scan
        (br_if $done (i32.eqz (i32.load8_u (local.get $p))))
        (local.set $p (i32.add (local.get $p) (i32.const 1)))
        (br_if $scan (i32.lt_u (local.get $p) (i32.const 8192)))))
    (i32.sub (local.get $p) (i32.const 4096)))

  (func (export "on_tick") (local $x f64) (local $y f64)
    (global.set $tick (i32.add (global.get $tick) (i32.const 1)))

    ;; ── 第一拍：读自己坐标，在 +5 格立场塔；立塔拍不移动（return）──
    (if (i32.eqz (global.get $tower_done))
      (then
        (call $get_pos)
        (local.set $y)
        (local.set $x)
        (drop
          (call $spawn
            (i32.const 4096)                                  ;; code_ptr
            (call $tower_len)                                 ;; code_len 运行时扫描
            (f64.add (local.get $x) (f64.const 5.0))          ;; 塔的 x = 我 +5
            (f64.const 0.0)                                   ;; 塔的 y（地面）
            (i32.const 8192)                                  ;; glyph "F"
            (i32.const 8256)                                  ;; color "lime"
            (f64.const 0.5)                                   ;; radius（第7参，默认 0.5）
            (f64.const 1.0)                                   ;; mass（第8参，默认 1.0）
            (i32.const 0)                                     ;; binding（第9参，0=unbound）
            (i32.const 0)                                     ;; motion（第10参，0=static 钉死塔）
            (f64.const 0.0)                                   ;; vx（第11参出生初速x）
            (f64.const 0.0)                                   ;; vy（第12参出生初速y）
            (i32.const 0)                                     ;; blob_ptr（第13参出生数据包，0=无包）
            (i32.const 0)                                     ;; blob_len（第14参，0=无包）
            (f64.const 3.0)                                   ;; field_radius（第15参：场域半径 3 格）
            (f64.const 1.0)                                   ;; field_gravity_scale（第16参：重力不变）
            (f64.const 1.95)                                  ;; field_drag_delta（第17参：+1.95=40 倍阻力）
            (f64.const 0.0)                                   ;; liquid_density（D-009 第18参，0=非液体）
            (f64.const 0.0)                                   ;; liquid_drag（第19参，0=无阻尼）
            (i32.const 0)                                     ;; link（D-011 第20参，0=无链放生）
            (f64.const 0.0)                                   ;; restitution（D-010 第21参，0=撞停默认）
            (f64.const 0.6)                                   ;; friction（第22参，0.6=默认摩擦）
            (i32.const 10000)                                 ;; lifespan（D-019 第 23 参，强制声明：>=1 才出生；长寿造物 10000）
            (i32.const 8704)))                                ;; name_ptr（D-020b 第24参，造物实名"护盾塔"）
        (i32.const 5) (call $log)                             ;; marker 5 = 立场塔信号
        (global.set $tower_done (i32.const 1))
        (return)))

    ;; ── 之后每拍：观察信号（marker 3，每 4 拍一次）──
    (if (i32.eqz (i32.rem_u (global.get $tick) (i32.const 4)))
      (then (i32.const 3) (call $log)))))
