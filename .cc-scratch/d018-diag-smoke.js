// D-018：smoke 逐拍诊断
const fs = require('fs');
const p = 'D:/projects/CodeWorld/.cc-wt-d018/bin/codeworld/src/main.rs';
let s = fs.readFileSync(p, 'utf8');
// 在 smoke 测试的 tick2 循环后加打印——找 "// tick2：造物弹命中靶身" 前
const anchor = '        // tick2：造物弹命中靶身（接触截断）-> on_contact 排队 attack（次拍结算）';
const i = s.indexOf(anchor);
if (i < 0) throw new Error('anchor not found');
const diag = [
'        {',
'            let sh = world.entities.values().find(|e| is_creation_shell(e));',
'            eprintln!("SMOKE after t1: shell={:?} target_alive={}",',
'                sh.map(|e| (e.pos(), e.velocity(), e.is_alive())),',
'                world.entities.get(&target).map(|e| e.is_alive()));',
'        }',
'',
].join('\r\n');
s = s.slice(0, i) + diag + s.slice(i);
// tick3 后加打印
const a2 = '        // tick3：attack 结算被盾拦截（弹 $age=1 仍存活——结算需要攻击者在册）';
const j = s.indexOf(a2);
if (j < 0) throw new Error('anchor2 not found');
const diag2 = [
'        {',
'            let sh = world.entities.values().find(|e| is_creation_shell(e));',
'            eprintln!("SMOKE after t2: shell={:?} target_alive={}",',
'                sh.map(|e| (e.pos(), e.velocity(), e.is_alive())),',
'                world.entities.get(&target).map(|e| e.is_alive()));',
'        }',
'',
].join('\r\n');
s = s.slice(0, j) + diag2 + s.slice(j);
// tick4 后加打印
const a3 = '        // 证据链3 命中：造物弹消亡（命中即亡，死亡清理移除）';
const k = s.indexOf(a3);
if (k < 0) throw new Error('anchor3 not found');
const diag3 = [
'        {',
'            let sh = world.entities.values().find(|e| is_creation_shell(e));',
'            eprintln!("SMOKE after t4: shell={:?} target_alive={} shield={}",',
'                sh.map(|e| (e.pos(), e.velocity(), e.is_alive())),',
'                world.entities.get(&target).map(|e| e.is_alive()),',
'                call_i32_export(&mut map, &target, "get_shield"));',
'        }',
'',
].join('\r\n');
s = s.slice(0, k) + diag3 + s.slice(k);
fs.writeFileSync(p, s);
console.log('smoke diag added');
