;; ============================================================
;; duel-demo——决斗游魂（D-005 刀0 决斗制攻杀示例，玩家文档）
;;
;; 这是什么：一个"登陆敌方沙箱"的攻方游魂模块示例。近战/造物弹命中敌方身体
;; 且 payload 非空时，世界不再换血——改为开决斗会话：你（游魂）以自己出厂
;; 携带的燃料赌注（=创造者的 fuel 配额，#047 继承制）在守方内存里持续读写，
;; 每拍一个窗口，胜负全由双方代码定（世界只当硬件，docs/design-duel.md）。
;;
;; 部署方式：把本模块字节作为 attack_at 的 payload 发出去（payload 从你自己的
;; memory 读取），命中无防御守方即本模块登陆。示例用直接调用三原语：
;;
;; 游魂窗口契约（docs/api.md「决斗制（D-005 刀0）」节）：
;;   env.duel_read(ptr, len, out_ptr) -> i32   读守方内存 -> 写进自己 [out_ptr,..)
;;                                             返回实际字节数，越界 -1
;;   env.duel_write(ptr, bytes_ptr, len) -> i32 把自己 [bytes_ptr, +len) 写进守方
;;                                             内存 ptr 处；返回字节数，越界 -1
;;   env.duel_round() -> i32                   当前窗口拍号（0 起）
;;   导出 on_duel() -> i32                     返回值即投票：1=宣布完成（会话
;;                                             正常收尾），其他=继续
;;
;; 写码注意（与日常模块的三点不同）：
;;   1. 游魂窗口里只有这三个 import——env.log/get_pos/spawn 一概没有
;;      （走私多余 import 的模块拒绝实例化=游魂崩溃，通信自由注入无门）。
;;      "log" 的等价物=把读到的字节落进自己的 memory（战利品架），随会话消亡；
;;   2. 你烧的是自己的出厂赌注（死油箱不重发）——守方每拍油箱重发而你不会，
;;      拖下去必然油尽被驱逐（防御成功）；干完就收工（on_duel 返回 1）最经济；
;;   3. 每拍重编译实例化（刀0 策略），复杂策略请在窗口内算完就走。
;;
;; 本示例做什么：每个窗口读守方内存头部 16 字节（侦察）→ 把读到的字节回写
;; 到守方 [1040,1056)（战果落位，世界可观察）→ 把签名章 "DUELWAS\0" 盖到
;; 守方 [2048,2056) → 第 3 号窗口之后返回 1 宣布完成（会话正常收尾）。
;; ============================================================
(module
  (import "env" "duel_read" (func $duel_read (param i32 i32 i32) (result i32)))
  (import "env" "duel_write" (func $duel_write (param i32 i32 i32) (result i32)))
  (import "env" "duel_round" (func $duel_round (result i32)))

  (memory (export "memory") 1)

  ;; 签名章（游魂自己的 data 段，经 duel_write 盖到守方内存）
  (data (i32.const 2048) "DUELWAS\00")

  (func (export "on_duel") (result i32)
    ;; 侦察：读守方内存头部 16 字节 → 自己的战利品架 [1024,1040)
    ;; （想偷魂就多读些、想夺舍就改身份区、想破坏就乱写——策略归你，
    ;;   世界只提供读写原语，读就是读写就是写）
    (drop (call $duel_read (i32.const 0) (i32.const 16) (i32.const 1024)))

    ;; 战果落位（可观察）：把读到的 16 字节写回守方 [1040,1056)
    ;; ——读→带→写全链路演示；静默潜伏请删掉这段（动静大小=战术选择）
    (drop (call $duel_write (i32.const 1040) (i32.const 1024) (i32.const 16)))

    ;; 签名章：把 data 段的 "DUELWAS\0" 盖到守方 [2048,2056)
    (drop (call $duel_write (i32.const 2048) (i32.const 2048) (i32.const 8)))

    ;; 第 3 号窗口之后收工：返回 1 = 宣布完成（会话正常收尾）
    ;; （永远返回 0 也合法——但每窗口都烧赌注，油尽时照样被驱逐）
    (if (result i32) (i32.gt_s (call $duel_round) (i32.const 2))
      (then (i32.const 1))
      (else (i32.const 0)))))
