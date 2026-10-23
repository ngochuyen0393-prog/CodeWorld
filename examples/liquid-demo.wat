;; ============================================================
;; liquid-demo——挖池人（D-009 液体出生声明家族示例，玩家文档）
;;
;; 这是什么：一个"进场后在 +5 格挖一个 5×3 水坑"的玩家角色示例。
;; 世界没有内置的水——水=你造出来的**液体造物**（spawn 第 18/19 参）：
;; 每拍立一块 1×1 的水方块，15 拍拼出一个 5 宽×3 高的水域。
;;
;; 部署方式：本文件放 players/ 目录即进场（或作为进场代码模板抄写）。
;;
;; 液体参数速查（docs/api.md「液体（D-009）」节）：
;;   第 18 参 liquid_density 域 [0.0, 5.0]，0=非液体（默认=普通造物）；
;;            >0 即声明"我的身体区域（格对齐 AABB）是液体"
;;   第 19 参 liquid_drag     域 [0.0, 5.0]，水中阻尼 k（每拍速度保留系数
;;            的 MC 语义：v ← v×k + 浮力改写重力；结算时有效保留系数钳 [0,1]）
;;
;; 世界只认数字不认名字（decisions.md #053）——"油 0.75 / 水 1.0 /
;; 盐水 1.1 / 蜂蜜 1.4 / 岩浆 2.5"是**社区预设黑话**，不是世界内置清单：
;; 你造 ρ=0.8 的"蓝油"、ρ=3.7 的"星核浆"完全合法，浮沉全由公式涌现
;;（g_final = g × (1 − ρ液/ρ物 × 浸入比)，木浮铁沉零特例无清单）。
;;
;; 液体造物三条特性：
;;   ① 纯布景无沙箱——液体是物理域不是居民，出生跳过沙箱创建（没有
;;      on_tick/life 可跑），但照常占世界实体名额（防刷水弹幕）；
;;   ② 水不会动——liquid 造物无视 motion 声明强制 static（格对齐 AABB），
;;      不做守恒水体（不流动不搬运，MC 1.13 教训）；
;;   ③ 照常被 scan/probe 探测——"我在水里"没有专属事件：你 scan 探到
;;      液体造物 + 自己坐标落在它 AABB 内 = 你在水里（状态机是玩家的
;;      功课：岸上/入水/半沉/全浸没/出水五态自己写）。
;;
;; 写码注意：spawn 每实体每拍限 1 个（15 块水=15 拍，别想一拍糊满）；
;; WAT 的 local 声明必须在函数体开头；多参数 import 调用用 locals+显式
;; 传参最稳（与 players/pioneer.wat 同款结构）。
;; ============================================================
(module
  (import "env" "log" (func $log (param i32)))
  (import "env" "get_pos" (func $get_pos (result f64 f64)))
  (import "env" "spawn" (func $spawn (param i32 i32 f64 f64 i32 i32 f64 f64 i32 i32 f64 f64 i32 i32 f64 f64 f64 f64 f64 i32 f64 f64 i32) (result i32)))

  (memory (export "memory") 1)

  ;; ── 水方块代码全文（单行，以 \00 结尾）：最简 life 模块——液体造物无沙箱
  ;;    不跑代码，这段只是 spawn 合法性校验的载体 ──
  (data (i32.const 4096) "(module (func (export \"life\") (result i32) (i32.const 1)))\00")

  ;; ── 水方块外观数据（NUL 结尾）：~ 字形 + blue（快照 entities 层照常渲染）──
  (data (i32.const 8192) "~\00")
  (data (i32.const 8256) "blue\00")

  (global $tick (mut i32) (i32.const 0))
  (global $pool_done (mut i32) (i32.const 0))

  (func (export "life") (result i32) (i32.const 1))

  ;; 内部工具：扫描 [4096..) 直到 NUL，返回水方块代码字节数
  (func $code_len (result i32)
    (local $p i32)
    (local.set $p (i32.const 4096))
    (block $done
      (loop $scan
        (br_if $done (i32.eqz (i32.load8_u (local.get $p))))
        (local.set $p (i32.add (local.get $p) (i32.const 1)))
        (br_if $scan (i32.lt_u (local.get $p) (i32.const 8192)))))
    (i32.sub (local.get $p) (i32.const 4096)))

  ;; 水方块代码长度（data 段扫描结果存全局，spawn 调用直接读）
  (global $wlen (mut i32) (i32.const 0))

  (func (export "on_tick") (local $x f64) (local $y f64) (local $n i32) (local $col i32) (local $row i32)
    (global.set $tick (i32.add (global.get $tick) (i32.const 1)))

    ;; 首拍：记水方块代码长度（运行时扫描，手数必错）
    (if (i32.eq (global.get $tick) (i32.const 1))
      (then (global.set $wlen (call $code_len))))

    ;; ── 第 1..15 拍：每拍立一块水，拼 5 宽 × 3 高水坑（底行先落，靠出生
    ;;    预检+落地悬停自然分层：上行块落到下行块顶被挡=悬停原位）──
    (if (i32.lt_u (global.get $tick) (i32.const 16))
      (then
        ;; 序号 n = tick-1 ∈ [0,15)：col = n % 5（东向），row = n / 5（层）
        (local.set $n (i32.sub (global.get $tick) (i32.const 1)))
        (local.set $col (i32.rem_u (local.get $n) (i32.const 5)))
        (local.set $row (i32.div_u (local.get $n) (i32.const 5)))
        (call $get_pos)
        (local.set $y)
        (local.set $x)
        (drop
          (call $spawn
            (i32.const 4096)                                  ;; code_ptr
            (global.get $wlen)                                ;; code_len
            (f64.add
              (f64.add (local.get $x) (f64.const 5.0))
              (f64.convert_i32_u (local.get $col)))           ;; 坑 x = 我 +5 + col
            (f64.convert_i32_u (local.get $row))              ;; 坑 y = row（0/1/2 层）
            (i32.const 8192)                                  ;; glyph "~"
            (i32.const 8256)                                  ;; color "blue"
            (f64.const 0.5)                                   ;; radius（第7参，static 不用它）
            (f64.const 1.0)                                   ;; mass（第8参）
            (i32.const 0)                                     ;; binding（第9参，0=unbound）
            (i32.const 0)                                     ;; motion（第10参——液体造物无视它，强制 static）
            (f64.const 0.0)                                   ;; vx（第11参）
            (f64.const 0.0)                                   ;; vy（第12参）
            (i32.const 0)                                     ;; blob_ptr（第13参，0=无包）
            (i32.const 0)                                     ;; blob_len（第14参，0=无包）
            (f64.const 0.0)                                   ;; field_radius（D-012 第15参，0=无场）
            (f64.const 1.0)                                   ;; field_gravity_scale（第16参）
            (f64.const 0.0)                                   ;; field_drag_delta（第17参）
            (f64.const 1.0)                                   ;; liquid_density（D-009 第18参：ρ=1.0 纯水）
            (f64.const 0.35)                                  ;; liquid_drag（第19参：k=0.35，入水一拍掉 65%）
            (i32.const 0)                                     ;; link（D-011 第20参，0=无链放生）
            (f64.const 0.0)                                   ;; restitution（D-010 第21参，0=撞停默认）
            (f64.const 0.6)                                   ;; friction（第22参，0.6=默认摩擦）
            (i32.const 10000)))                               ;; lifespan（D-019 第 23 参，强制声明：>=1 才出生；长寿造物 10000）
        (if (i32.eq (global.get $tick) (i32.const 15))
          (then
            (i32.const 5) (call $log)                         ;; marker 5 = 水坑完工信号
            (global.set $pool_done (i32.const 1))
            (return)))
        (return)))

    ;; ── 水坑完工后：每 4 拍观察信号（marker 3）──
    (if (i32.eqz (i32.rem_u (global.get $tick) (i32.const 4)))
      (then (i32.const 3) (call $log)))))
