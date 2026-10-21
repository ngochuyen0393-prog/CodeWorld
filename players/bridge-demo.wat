;; ============================================================
;; 桥演示（bridge-demo）——D-001b 外桥最小实测生物
;; 性格：离不开外脑的朝圣者。每拍写信问外脑"往哪走"（bridge_send），
;;       外脑回信里的数字当步数（{"dx":3} = 连走 3 拍向东），走完再等下一封。
;; 行为时间线（世界开桥 CODEWORLD_BRIDGE=1 + bridges.json 1 号端点在线时）：
;;   每拍      : bridge_send(1, "WHERE?", 6) 发信——返回码直接 log 可见
;;               （0=已排队 1=无此端点 2=payload 超限 3=本拍已发/在途满 4=桥未启用）
;;   回信拍    : bridge_on_reply 扫回信第一个数字当步数（log 5）；无数字 log 9
;;   有步数时  : move_by(1.0, 0.0) 向东走一步 + log 6，步数减 1
;;   世界没开桥: bridge_send 恒返 4（log 4）——原地在场心跳，life 恒 1
;; 机制备忘（docs/api.md bridge_send/bridge_on_reply 词条）：
;;   - port 是保险库别名，沙箱永远拿不到 URL（凭证不进沙箱铁律）
;;   - 回信字节由世界 grow 新页写入（不覆盖玩家数据），异步跨拍投递
;;   - 每拍 1 发限流 + 在途上限 4（世界节律定天花板，docs/game-menu.md 1.7）
;; 下线语义：删掉本文件 = 你下线（残影5拍消散）
;; ============================================================
(module
  (import "env" "bridge_send" (func $bridge_send (param i32 i32 i32) (result i32)))
  (import "env" "move_by" (func $move_by (param f64 f64) (result i32)))
  (import "env" "log" (func $log (param i32)))

  (global $steps (mut i32) (i32.const 0))  ;; 剩余步数（回信里的数字）
  (memory (export "memory") 1)
  (data (i32.const 0) "WHERE?")            ;; 信体：6 字节（payload 上限 8KB 内）

  ;; 扫 [ptr, ptr+len) 找第一个 ASCII 数字，返回 0-9；找不到返回 0
  (func $scan_digit (param $ptr i32) (param $len i32) (result i32)
    (local $i i32) (local $c i32)
    (block $done
      (loop $loop
        (br_if $done (i32.ge_u (local.get $i) (local.get $len)))
        (local.set $c (i32.load8_u (i32.add (local.get $ptr) (local.get $i))))
        (if (i32.and (i32.ge_u (local.get $c) (i32.const 48))
                     (i32.le_u (local.get $c) (i32.const 57)))
          (then (return (i32.sub (local.get $c) (i32.const 48)))))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $loop)))
    i32.const 0)

  (func (export "life") (result i32) i32.const 1)

  (func (export "on_tick")
    ;; 每拍发信（返回码进日志：marker=0 成功排队 / 4=桥未启用 / 其余见词条）
    (call $log (call $bridge_send (i32.const 1) (i32.const 0) (i32.const 6)))
    ;; 步数未耗尽：向东走一步（log 6），步数减 1——假服务器回 {"dx":3}，
    ;; 造物连走 3 拍 = 外脑指令改变行为的肉眼证据
    (if (i32.gt_u (global.get $steps) (i32.const 0))
      (then
        (drop (call $move_by (f64.const 1.0) (f64.const 0.0)))
        (call $log (i32.const 6))
        (global.set $steps (i32.sub (global.get $steps) (i32.const 1))))))

  ;; 回信投递（世界 collect 阶段调用；缺省导出=收不到回信，本造物选择收信）
  (func (export "bridge_on_reply") (param $port i32) (param $ptr i32) (param $len i32)
    (local $d i32)
    (local.set $d (call $scan_digit (local.get $ptr) (local.get $len)))
    (if (i32.eqz (local.get $d))
      (then (call $log (i32.const 9)))    ;; 回信里没有数字：log 9
      (else
        (call $log (i32.const 5))          ;; 收到指令：log 5
        (global.set $steps (local.get $d))))))
