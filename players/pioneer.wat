;; ============================================================
;; 拓荒者（pioneer）——你的第一个"真玩家"角色
;; 性格：谨慎的拓荒者。进场第一拍在 +5 格立一座哨塔（立塔那拍不动），
;;       之后每拍向东走一步，每 4 拍看一眼四周（log 3 = 拓荒观察）。
;; 行为时间线（进场后）：
;;   tick 1  : 在 (+5, 0) 立哨塔（哨塔代码内嵌本文件 data 段，单行），本拍不移动
;;   tick 2+ : 每拍向东走一步
;;   每 4 拍 : log(3) 观察信号
;; 占位注意（实体占位制 #021）：一格一物。塔离自己 +5 并立塔拍不移动，
;;   彻底错开出生格，避免刚进场就撞上自己的塔（撞上=移动被挡，站在原地）。
;; 下线语义：删掉本文件 = 你下线（残影5拍消散）；哨塔是造物，会一直站着
;; 注意：data 段字符串内不能有裸换行（WAT 语法）；WAT 是栈机，
;;       多参数 import 调用（如 spawn 6 参数）用 locals+显式传参最稳
;; ============================================================
(module
  (import "env" "log" (func $log (param i32)))
  (import "env" "get_pos" (func $get_pos (result f64 f64)))
  (import "env" "move_by" (func $move_by (param f64 f64) (result i32)))
  (import "env" "spawn" (func $spawn (param i32 i32 f64 f64 i32 i32 f64 f64 i32 i32 f64 f64 i32 i32 f64 f64 f64 f64 f64) (result i32)))

  (memory (export "memory") 1)

  ;; ── 哨塔代码全文（单行，以 \00 结尾）──────────────────
  (data (i32.const 4096) "(module (import \"env\" \"visible_count\" (func $vc (result i32))) (import \"env\" \"log\" (func $log (param i32))) (memory (export \"memory\") 1) (func (export \"life\") (result i32) i32.const 1) (func (export \"on_tick\") (if (i32.gt_s (call $vc) (i32.const 0)) (then (i32.const 2) (call $log)))))\00")

  ;; ── 外观数据（NUL 结尾） ─────────────────────────────
  (data (i32.const 8192) "T\00")
  (data (i32.const 8256) "cyan\00")

  (global $tick (mut i32) (i32.const 0))
  (global $tower_done (mut i32) (i32.const 0))

  (func (export "life") (result i32) i32.const 1)

  ;; 内部工具：扫描 [4096..) 直到 NUL，返回哨塔代码字节数
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

    ;; ── 第一拍：读自己坐标，在 +5 格立哨塔；立塔拍不移动（return）──
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
            (i32.const 8192)                                  ;; glyph "T"
            (i32.const 8256)                                  ;; color "cyan"
            (f64.const 0.5)                                   ;; radius（#030 第7参，默认 0.5）
            (f64.const 1.0)                                   ;; mass（#034 第8参，默认 1.0）
            (i32.const 0)                                     ;; binding（#034修订四 第9参，0=unbound 同生不共死）
            (i32.const 0)                                     ;; motion（D-003 第10参，0=static 钉死塔；1=dynamic 全活）
            (f64.const 0.0)                                   ;; vx（第11参出生初速x，0=零速落地不动）
            (f64.const 0.0)                                   ;; vy（第12参出生初速y）
            (i32.const 0)                                     ;; blob_ptr（第13参出生数据包指针，0=无包）
            (i32.const 0)                                     ;; blob_len（第14参出生数据包长度，0=无包）
            (f64.const 0.0)                                   ;; field_radius（D-012 第15参，0=无场）
            (f64.const 1.0)                                   ;; field_gravity_scale（第16参，1=中性缩放）
            (f64.const 0.0)                                   ;; field_drag_delta（第17参，0=不改写）
            (f64.const 0.0)                                   ;; liquid_density（D-009 第18参，0=非液体）
            (f64.const 0.0)))                                 ;; liquid_drag（第19参，0=无阻尼）
        (i32.const 5) (call $log)                             ;; marker 5 = 立塔信号
        (global.set $tower_done (i32.const 1))
        (return)))

    ;; ── 每拍：向东走一步 ──
    (drop (call $move_by (f64.const 1.0) (f64.const 0.0)))

    ;; ── 每 4 拍：观察信号 ──
    (if (i32.eqz (i32.rem_u (global.get $tick) (i32.const 4)))
      (then (i32.const 3) (call $log)))))
