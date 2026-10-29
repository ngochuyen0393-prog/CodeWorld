;; 空白木桩（新手模板 1：什么都不做，只活着——你的起跑线）
;; 你的代码在 players/<你的名字>.wat：改这个文件 = 改你的命运。
;; 心跳 marker 1：控制台「我的输出」里每拍亮一下——证明你活着。
(module
  (import "env" "log" (func $log (param i32)))
  (memory (export "memory") 1)
  (func (export "life") (result i32) i32.const 1)
  (func (export "on_tick")
    (i32.const 1) (call $log)))
