// D-018：hunter_shoots 诊断 v2（接触对+攻击队列）
const fs = require('fs');
const p = 'D:/projects/CodeWorld/.cc-wt-d018/bin/codeworld/src/main.rs';
let s = fs.readFileSync(p, 'utf8');
const anchor = '            saw_projectile |= world.entities.values().any(|e| is_creation_shell(e) && e.is_alive());';
const i = s.indexOf(anchor);
if (i < 0) throw new Error('anchor not found');
const diag = anchor + '\r\n' + [
'            eprintln!(',
'                "DBG t={} shells={} victim_alive={} victim_pos={:?} touching={:?}",',
'                world.tick(),',
'                world.entities.values().filter(|e| is_creation_shell(e)).count(),',
'                world.entities.get(&victim).map(|e| e.is_alive()),',
'                world.entities.get(&victim).map(|e| e.pos()),',
'                sandbox_map.last_touching,',
'            );',
].join('\r\n');
s = s.slice(0, i + anchor.length) + '\r\n' + diag + s.slice(i + anchor.length);
fs.writeFileSync(p, s);
console.log('diag v2 added');
