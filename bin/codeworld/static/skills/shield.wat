;; 护盾场塔（D-020a 技能栏槽位 3 模板）：力场三参由服务端定装（半径 4/
;; 阻力改写 +5——糖浆场，域内寸步难行），本体=纯被动场源（life=1 钉死）。
;; 场的可表达权（on_field 应答改阻力）留给你自己写——模板给最笨的场。
(module
  (memory (export "memory") 1)
  (func (export "life") (result i32) (i32.const 1)))
