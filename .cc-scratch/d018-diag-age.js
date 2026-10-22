// D-018：弹头加 get_age/get_fired 观测 + smoke 打印
const fs = require('fs');
const p = 'D:/projects/CodeWorld/.cc-wt-d018/bin/codeworld/src/main.rs';
let s = fs.readFileSync(p, 'utf8');
const anchor = '  (func (export "on_contact") (param f64 f64)';
if (!s.includes(anchor)) throw new Error('wat anchor not found');
s = s.replace(anchor,
'  (func (export "get_age") (result i32) global.get $age)\r\n' +
'  (func (export "get_fired") (result i32) global.get $fired)\r\n' +
anchor);
// smoke t4 打印 age/fired
const a3 = '                call_i32_export(&mut map, &target, "get_shield"));';
if (!s.includes(a3)) throw new Error('smoke anchor not found');
s = s.replace(a3, a3 + '\r\n' + [
'                if let Some(sh) = world.entities.values().find(|e| is_creation_shell(e)) {',
'                    eprintln!("SMOKE diag age={} fired={}",',
'                        call_i32_export(&mut map, &sh.id, "get_age"),',
'                        call_i32_export(&mut map, &sh.id, "get_fired"));',
'                }',
].join('\r\n'));
fs.writeFileSync(p, s);
console.log('age diag added');
