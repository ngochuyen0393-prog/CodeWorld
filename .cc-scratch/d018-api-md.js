// D-018：api.md 三处同步（shoot 词条退役 / visible_count 痕迹句 / probe -3 段）
const fs = require('fs');
const p = 'D:/projects/CodeWorld/.cc-wt-d018/docs/api.md';
let s = fs.readFileSync(p, 'utf8');
// 按 CRLF 还是 LF？先探测
const crlf = s.includes('\r\n');
const NL = crlf ? '\r\n' : '\n';
const J = a => a.join(NL);

// 1. shoot 词条（153-177 一带）整段替换为退役说明
const shootStart = s.indexOf('### `shoot(vx: f64, vy: f64, mass: f64, payload_ptr: i32, payload_len: i32, binding: i32) -> i32`');
const visStart = s.indexOf('### `visible_count() -> i32`');
if (shootStart < 0 || visStart < 0) throw new Error('shoot bounds');
const retired = J([
'### `shoot` —— 已退役（历史锚）',
'',
'**`shoot` 已从世界 API 退役**（decisions.md #045，2026-10-20 立法 / D-018 执行 2026-10-22）：世界不配发身体——制式弹丸（0.15 圆点 + 世界弹道管线）是"世界造车"，违反 #038 修路不跑车 / #011 零预设 / #041 三位一体。远程发射的替代路径全部已就位：**一体式**（即造即射）= `spawn` 出生初速（第 11/12 参）+ 接触通道代码攻（`on_contact` → `attack_at`）；**分离式**（炮管装填）= `field_pulse` 物理推存量。完整组装教学见 **docs/newbie-first-thrower.md（投掷器模板第一课）**。老代码里的 `shoot` 调用不再编译通过（import 已移除）。',
'',
]);
s = s.slice(0, shootStart) + retired + s.slice(visStart);

// 2. visible_count 的痕迹句
const oldVis = '眼睛列表长度：视口内的其他存活实体数（按 EntityId 升序稳定排序，不含自己）。**含发射痕迹**（#034 修订三：别人开火的痕迹也在列表里，坐标可见——但那是标记不是实体，probe 才能认出它是谁的痕迹）。';
const newVis = '眼睛列表长度：视口内的其他存活实体数（按 EntityId 升序稳定排序，不含自己）。';
if (!s.includes(oldVis)) throw new Error('visible_count sentence');
s = s.replace(oldVis, newVis);

// 3. probe 的 -3 段
const oldProbe = J([
'探测可见实体（坐标寻址，只读，不扣你的手续费）。request 字节从你的 memory 读取；对方的 `on_probe` 返回 + **24 字节被动体徵尾块**写入你的 memory `[resp_ptr, ...)`。同坐标实体与痕迹并存时**实体优先**。',
'',
'返回值：',
'- `>= 0`：实际写入字节数（response + 24，超过 `resp_cap` 截断）',
'- `-1`：坐标无可见实体 / 响应不可写',
'- `-2`：目标没有 `on_probe` 导出（响应位置已写默认被动体徵）',
'- `-3`：**坐标上是发射痕迹**（#034 修订三：对方在此发射过弹丸）。响应位置写入 **24 字节痕迹证据**（不建沙箱、不触发对方 on_probe）：',
'',
'| 偏移 | 含义 |',
'|---|---|',
'| `[0..16]` | 发射者 EntityId（128 位 Uuid，u128 LE） |',
'| `[16..24]` | 发射拍号（u64 LE） |',
'',
'"此处发射过弹丸、是谁、什么时候"——痕迹追踪反击（哨塔网接力 probe）从这里起步。',
'',
]);
const newProbe = J([
'探测可见实体（坐标寻址，只读，不扣你的手续费）。request 字节从你的 memory 读取；对方的 `on_probe` 返回 + **24 字节被动体徵尾块**写入你的 memory `[resp_ptr, ...)`。',
'',
'返回值：',
'- `>= 0`：实际写入字节数（response + 24，超过 `resp_cap` 截断）',
'- `-1`：坐标无可见实体 / 响应不可写',
'- `-2`：目标没有 `on_probe` 导出（响应位置已写默认被动体徵）',
'',
'（历史注记：`-3` 发射痕迹返回值随 #045 弹丸退役一并退役——造物弹天然在 visible 列表暴露，独立痕迹机制已移除；老文档提到 probe 命中痕迹坐标拿发射者身份的玩法，造物弹时代=对弹本身 probe。）',
'',
]);
if (!s.includes(oldProbe)) throw new Error('probe block');
s = s.replace(oldProbe, newProbe);

fs.writeFileSync(p, s);
console.log('api.md synced; CRLF=' + crlf);
