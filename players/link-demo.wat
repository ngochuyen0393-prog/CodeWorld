;; ============================================================
;; 链演示（link-demo）——D-011 世界内链路段最小实测生物：风筝手
;; 性格：放风筝的人。第一拍放一只活链侦察弹（spawn 第 20 参 link=1），
;;       每 8 拍用 command 广播转向方向（f64 8 字节，东 1.0 / 西 -1.0），
;;       侦察弹收到改 move_by 方向、每拍 report 自己坐标回来。
;; 行为时间线（无需任何 config，纯世界内通道）：
;;   第 1 拍    : spawn 侦察弹（+2 格，link=1 活链）——marker 5 = 放飞信号
;;   每 8 拍    : 方向翻转并 command 广播（返回值=名下活链数，1=连着）
;;   回传拍     : on_report 收侦察弹坐标——marker 7 = 收到情报
;;   侦察弹侧   : on_command 收方向（f64 直读）+ 每拍 report 坐标 + move_by 走
;; 机制备忘（docs/api.md「世界内链路段（D-011）」节）：
;;   - 链是活电话：母体死=链断（造物活着），广播制无编号（孩子自己过滤）
;;   - 两拍节律：拍 N command / 拍 N+1 弹的 on_command 被调 / 拍 N+2 弹走新向
;;   - 每拍各 1 次限流（command/report 各自独立记账）
;; 下线语义：删掉本文件 = 你下线（残影5拍消散）；链随实体字段走，夺剑不夺链
;; ============================================================
(module
  (import "env" "log" (func $log (param i32)))
  (import "env" "get_pos" (func $get_pos (result f64 f64)))
  (import "env" "command" (func $command (param i32 i32) (result i32)))
  (import "env" "spawn" (func $spawn (param i32 i32 f64 f64 i32 i32 f64 f64 i32 i32 f64 f64 i32 i32 f64 f64 f64 f64 f64 i32 f64 f64 i32) (result i32)))

  (memory (export "memory") 1)

  ;; ── 侦察弹代码全文（单行，以 \00 结尾）：活链+每拍 report 坐标+收向走 ──
  ;; on_command(ptr, len) 的信字节由世界 grow 新页写入（offset=旧末尾）——
  ;; 母体广播的是 8 字节 f64 方向，f64.load 直读当方向用（世界不解释内容）
  (data (i32.const 4096) "(module (import \"env\" \"report\" (func $rp (param i32 i32) (result i32))) (import \"env\" \"get_pos\" (func $gp (result f64 f64))) (import \"env\" \"move_by\" (func $mb (param f64 f64) (result i32))) (import \"env\" \"log\" (func $lg (param i32))) (global $dir (mut f64) (f64.const 1.0)) (memory (export \"memory\") 1) (func (export \"life\") (result i32) (i32.const 1)) (func (export \"on_tick\") (local $x f64) (local $y f64) (call $gp) (local.set $y) (local.set $x) (f64.store (i32.const 2048) (local.get $x)) (f64.store (i32.const 2056) (local.get $y)) (drop (call $rp (i32.const 2048) (i32.const 16))) (drop (call $mb (global.get $dir) (f64.const 0.0)))) (func (export \"on_command\") (param $p i32) (param $l i32) (global.set $dir (f64.load (local.get $p))) (i32.const 6) (call $lg)))\00")

  ;; ── 侦察弹外观数据（NUL 结尾）─────────────────────────
  (data (i32.const 8192) "K\00")
  (data (i32.const 8256) "cyan\00")

  (global $tick (mut i32) (i32.const 0))
  (global $kite_done (mut i32) (i32.const 0))
  (global $dir (mut f64) (f64.const 1.0))

  (func (export "life") (result i32) (i32.const 1))

  ;; 内部工具：扫描 [4096..) 直到 NUL，返回侦察弹代码字节数
  (func $scout_len (result i32)
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

    ;; ── 第一拍：读自己坐标，在 +2 格放活链侦察弹；放飞拍不广播（return）──
    (if (i32.eqz (global.get $kite_done))
      (then
        (call $get_pos)
        (local.set $y)
        (local.set $x)
        (drop
          (call $spawn
            (i32.const 4096)                                  ;; code_ptr
            (call $scout_len)                                 ;; code_len 运行时扫描
            (f64.add (local.get $x) (f64.const 2.0))          ;; 弹的 x = 我 +2
            (f64.const 0.0)                                   ;; 弹的 y（地面）
            (i32.const 8192)                                  ;; glyph "K"
            (i32.const 8256)                                  ;; color "cyan"
            (f64.const 0.3)                                   ;; radius（第7参，小弹）
            (f64.const 0.5)                                   ;; mass（第8参）
            (i32.const 0)                                     ;; binding（第9参，0=unbound——链管通信契约管生死）
            (i32.const 0)                                     ;; motion（第10参，0=static 钉死走 SetPos）
            (f64.const 0.0)                                   ;; vx（第11参）
            (f64.const 0.0)                                   ;; vy（第12参）
            (i32.const 0)                                     ;; blob_ptr（第13参，0=无包）
            (i32.const 0)                                     ;; blob_len（第14参，0=无包）
            (f64.const 0.0)                                   ;; field_radius（第15参，0=无场）
            (f64.const 1.0)                                   ;; field_gravity_scale（第16参）
            (f64.const 0.0)                                   ;; field_drag_delta（第17参）
            (f64.const 0.0)                                   ;; liquid_density（第18参，0=非液体）
            (f64.const 0.0)                                   ;; liquid_drag（第19参，0=无阻尼）
            (i32.const 1)                                     ;; link（第20参，1=活链——风筝线的诞生）
            (f64.const 0.0)                                   ;; restitution（第21参，D-010，0=撞停默认）
            (f64.const 0.6)                                   ;; friction（第22参，D-010，0.6=默认）
            (i32.const 10000)))                               ;; lifespan（D-019 第 23 参，强制声明：>=1 才出生；长寿造物 10000）
        (i32.const 5) (call $log)                             ;; marker 5 = 放飞信号
        (global.set $kite_done (i32.const 1))
        (return)))

    ;; ── 每 8 拍：翻转方向并广播（f64 8 字节直传，侦察弹 f64.load 直读）──
    ;;    返回值=名下活链数（1=线还连着；0=断了/没有——放生或弹死）
    (if (i32.eqz (i32.rem_u (global.get $tick) (i32.const 8)))
      (then
        (global.set $dir (f64.neg (global.get $dir)))
        (i32.const 0) (global.get $dir) (f64.store)
        (drop (call $command (i32.const 0) (i32.const 8))))))

  ;; 收侦察弹回传坐标（from=弹 id 低 64 位，坐标 16 字节在 [p, p+16)）——
  ;; marker 7 = 收到情报（风筝流的"眼睛"：母体足不出户知前线）
  (func (export "on_report") (param $from i64) (param $p i32) (param $l i32)
    (i32.const 7) (call $log)))
