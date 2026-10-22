# 投掷器模板第一课：拼一个会扔东西的造物

> 写给未来的造物玩家。读完这课，你会拥有一台每 8 拍自动向最近敌人投掷子弹的投掷器——世界上第一个"远程武器"，零件全是公开原语，没有一条世界特权。
>
> 历史注记在最后（§5）：为什么世界不再直接给你 `shoot`。

---

## 一、完整模板（可直接抄）

一体式投掷器：子弹代码内嵌在你的代码里，出生即带初速飞出。抄下面整段，改三个地方就能用（见注释）：

```wat
(module
  (import "env" "log" (func $log (param i32)))
  (import "env" "get_pos" (func $get_pos (result f64 f64)))
  (import "env" "scan" (func $scan (param i32 i32) (result i32)))
  (import "env" "move_by" (func $move_by (param f64 f64) (result i32)))
  (import "env" "attack_at" (func $attack_at (param f64 f64 i32 i32) (result i32)))
  (import "env" "spawn" (func $spawn
    (param i32 i32 f64 f64 i32 i32 f64 f64 i32 i32 f64 f64 i32 i32 f64 f64 f64 f64 f64 i32)
    (result i32)))
  (memory (export "memory") 1)
  ;; ↓↓↓ 改这里 1：子弹代码（data 段 [4096..)，别与 scan 缓冲 [0..16) 重叠）
  (data (i32.const 4096) "(module\0a  (import \"env\" \"attack_at\" (func $attack (param f64 f64 i32 i32) (result i32)))\0a  (memory (export \"memory\") 1)\0a  (global $fired (mut i32) (i32.const 0))\0a  (global $age (mut i32) (i32.const 0))\0a  (func (export \"life\") (result i32)\0a    (if (global.get $fired)\0a      (then\0a        (global.set $age (i32.add (global.get $age) (i32.const 1)))\0a        (if (i32.ge_u (global.get $age) (i32.const 2))\0a          (then (i32.const 0) (return)))))\0a    (i32.const 1))\0a  (func (export \"on_contact\") (param f64 f64)\0a    (if (global.get $fired) (then (return)))\0a    (i32.const 1) (global.set $fired)\0a    (drop (call $attack (local.get 0) (local.get 1) (i32.const 0) (i32.const 0)))))\00")
  ;; ↓↓↓ 改这里 2：子弹字形与颜色（data 段 [16384]/[16448]）
  (data (i32.const 16384) ".\00")
  (data (i32.const 16448) "yellow\00")
  (global $ticks (mut i32) (i32.const 0))
  (func (export "on_tick")
    (local $ex f64) (local $ey f64) (local $mx f64) (local $my f64)
    (local $dx f64) (local $dy f64) (local $dist f64)
    (local $ux f64) (local $uy f64)
    global.get $ticks i32.const 1 i32.add global.set $ticks
    ;; 扫描最近敌人；没有就待机
    (if (i32.eqz (call $scan (i32.const 0) (i32.const 8)))
      (then (call $log (i32.const 0)) (return)))
    (local.set $ex (f64.load (i32.const 0)))
    (local.set $ey (f64.load (i32.const 8)))
    (call $get_pos)
    (local.set $my)
    (local.set $mx)
    (local.set $dx (f64.sub (local.get $ex) (local.get $mx)))
    (local.set $dy (f64.sub (local.get $ey) (local.get $my)))
    (local.set $dist (f64.sqrt
      (f64.add (f64.mul (local.get $dx) (local.get $dx))
               (f64.mul (local.get $dy) (local.get $dy)))))
    ;; 每 8 拍投掷一发（装填拍到此为止——本拍移动会带偏弹道）
    (if (i32.eqz (i32.rem_u (global.get $ticks) (i32.const 8)))
      (then
        (call $log (i32.const 12))
        (local.set $ux (f64.div (local.get $dx) (local.get $dist)))
        (local.set $uy (f64.div (local.get $dy) (local.get $dist)))
        ;; ↓↓↓ 改这里 3：子弹长度 = data 段 [4096] 解码后的准确字节数（转义还原成原文再数，见 §4 坑 1）
        (drop (call $spawn
          (i32.const 4096)          ;; 1  code_ptr：子弹代码指针
          (i32.const 676)           ;; 2  code_len：子弹代码字节数（数准！截断/超长都编译失败；见 §4 坑 1。本值=上方示例解码后实测，自己改了代码要重数）
          (f64.add (local.get $mx) (f64.mul (local.get $ux) (f64.const 0.75)))  ;; 3  x：出生点（沿发射方向外推出膛，见 §三）
          (f64.add (local.get $my) (f64.mul (local.get $uy) (f64.const 0.75)))  ;; 4  y
          (i32.const 16384)         ;; 5  glyph_ptr：字形指针
          (i32.const 16448)         ;; 6  color_ptr：颜色指针
          (f64.const 0.15)          ;; 7  radius：子弹半径（小圆点=难被打中）
          (f64.const 0.5)           ;; 8  mass：子弹质量（轻弹弯垂/重弹平直，自己调手感）
          (i32.const 0)             ;; 9  binding 0=unbound
          (i32.const 1)             ;; 10 motion 1=dynamic（全活，受完整物理——必填！）
          (f64.div (f64.sub (local.get $dx) (f64.const 0.75)) (f64.const 1.805))  ;; 11 vx：两拍回弧水平分量
          (f64.const 9.53)          ;; 12 vy：出生初速——弹道的心脏
          (i32.const 0) (i32.const 0)   ;; 13/14 出生数据包（无）
          (f64.const 0.0)           ;; 15 field_radius（力场三参：无场）
          (f64.const 1.0)           ;; 16 field_gravity_scale（1.0=不改写重力）
          (f64.const 0.0)           ;; 17 field_drag_delta（0=不改写阻力）
          (f64.const 0.0)           ;; 18 liquid_density（0=非液体默认）
          (f64.const 0.0)           ;; 19 liquid_drag（0=无水中阻尼）
          (i32.const 0)))           ;; 20 link（D-011，0=无链放生；1=活链可控+可传）
        (return)))
    ;; 贴身机会刀（投掷器也能近战）
    (if (f64.le (local.get $dist) (f64.const 1.0))
      (then
        (call $log (i32.const 5))
        (drop (call $attack_at (local.get $ex) (local.get $ey)
                               (i32.const 0) (i32.const 0)))
        (return)))
    ;; 观望游走（自己写自己的性格）
    (call $log (i32.const 4))
    (if (i32.eqz (i32.and (global.get $ticks) (i32.const 1)))
      (then (drop (call $move_by (f64.const 0.5) (f64.const 0.0))))
      (else (drop (call $move_by (f64.const -0.5) (f64.const 0.0))))))
  (func (export "life") (result i32) i32.const 1))
```

*(实战范例：世界的 H 搅局者就是这套模板——`bin/codeworld/src/main.rs` 里搜 `hunter_wat`。)*

---

## 二、组装三步（模板在干什么）

**第 1 步：造物身体（spawn 参数）。**
子弹是一个真造物：`spawn` 第 7 参给半径（小圆点 0.15 难被打中）、第 8 参给质量（轻弹被阻力吃速度、短距弯垂；重弹平直远）、第 9 参绑定契约、**第 10 参 `motion=1` 必填**——dynamic 才受完整物理（重力/阻力/CCD），static 是钉死的桩，永远飞不出去。子弹代码 = data 段里那坨转义文本：它没有 `on_tick`（出生即飞，飞行归物理），只有 `on_contact`（撞到东西时被世界叫醒）。

**第 2 步：给它初速（第 11/12 参）。**
出生动量是物理事实，不是攻击预设。世界极速 25 格/拍封顶（恰等放行），超过拒绝。初速怎么拼出弹道？这世界的弹道每拍是：`vy += 重力(-9.8)` → `v -= 阻力×v/mass` → 极速钳制 → **位移 = (本拍初速 + 本拍末速) / 2**（匀加速解析解）。想要两拍回弧打 10 格外的敌人？H 的实测系数：`vx = (距离-0.75)/1.805`、`vy = +9.53`（mass 0.5 时）。参数是生物自己拼的，打不中 = 参数没拼对——弹道涌现，框架不管。

**第 3 步：接触结算（on_contact 代码攻）。**
子弹撞上任何身体（相切即接触，CCD 扫掠保证高速弹不隧穿），世界调用它的 `on_contact(other_x, other_y)`——模板里子弹对来者坐标 `attack_at` 出一刀（空 payload = 默认击杀模块）。近炸/分裂/追踪全自由：想让子弹炸开 = 在 `on_contact` 里再 spawn 几颗小弹；想追踪 = 写 `on_tick` 修正速度。世界零过滤，双方都收通知（被撞者也会收到——它可以选择反杀这颗弹，#038 沙箱通道）。

---

## 三、发射点外推（出膛技巧）

**问题**：出生点与任何既有身体重叠会被拒绝（#030 出生预检）。你在 (10, 0.5)，子弹出生在 (10, 0.5)——撞你自己的身体，spawn 失败。

**解法**：出生点沿**发射方向**前移：

```
出生点 = 我的位置 + 发射方向 × (我的半径 + 子弹半径 + 0.1)
```

0.1 是余量。这是物理诚实的外推——同一拍内子弹从你的身体边缘出膛，不是瞬移。模板里的 `(f64.add $mx (f64.mul $ux 0.75))` 就是它（0.5 + 0.15 + 0.1 = 0.75）。**注意**：外推会吃掉一段射程——初速里的"距离"要按出生点到目标的剩余距离算（模板 `vx` 里的 `-0.75`）。

**同理**：想向后发射？出生点在身后 0.75 格——照飞（CCD 没有发射者豁免，出膛即离身就不会打自己）。

---

## 四、常见坑

1. **子弹代码字节数数错** → `spawn` 编译失败，弹不出生（世界日志有 `[process_spawns] 造物 WAT 编译失败`）。要数的是**解码后**的字节数：世界从内存读到的是转义还原后的原文——`\0a` 解回一个换行、`\"` 解回一个引号（各算 1 字节），结尾 `\00` 是 NUL 哨兵不算。把 data 段转义文本当字面数（本模板字面 719）或把哨兵算进去（677）都错，正确值 = **676**（模板原文长度）。别目测，写段小脚本把 data 段文本先解码再数（转义规则：`\"`→引号、`\\`→反斜杠、`\hh` 两位十六进制→对应字节）。
2. **忘写 `motion=1`** → 子弹是 static 桩，出生即钉死。
3. **忘写出生点外推** → spawn 撞自己身体被拒（日志 `出生点与现有身体重叠`）。
4. **scan 缓冲 [0..16) 与子弹代码 data 段重叠** → 子弹代码被坐标覆盖，编译失败。子弹代码放 [4096..)。
5. **初速超 25** → 拒绝返回 0。想要更远 = 加质量（阻力衰减少）不是蛮加速度。
6. **每拍 1 次 spawn 限流** → 一拍只能造一颗（含所有造物）。想要弹幕 = 造一台多发炮台而不是一拍多弹。

---

## 五、历史注记：为什么没有 `shoot` 了

世界曾经直接给过一个 `shoot(vx, vy, mass, payload, binding)`——你调一下，世界亲手造一颗制式弹丸（0.15 圆点、世界弹道管线、30 拍寿命）替你飞。2026-10-20 它被退役了（decisions.md #045）。

原因一句话：**世界修路，不跑车**。攻击通道（`attack_at` = 纯投递判定，不造实体）是世界的事；造一颗弹丸的身体是世界在配发武器——它有身体却没有沙箱，是世界立法里的身份例外。宪法补全后它成了违宪配发，换血退役。

退役不删除：`shoot` 转位为**新手教学模板**（就是本文档）。你以前用 `shoot` 做的每一件事，现在用"spawn + 出生初速 + on_contact"三件套拼出来——而且更强了：子弹有沙箱、有代码、可以追踪、可以分裂、可以投降、可以在命中前给你回信。世界只提供物理与通道，**武器是什么样子，由你的代码决定**。

（分离式路线：炮身与炮弹分开、靠 `field_pulse` 物理推膛内炮弹——适合可装填的炮台，见 api.md `field_pulse` 节。）
