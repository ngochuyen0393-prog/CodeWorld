// D-018：hunter_shoots 临时诊断打印
const fs = require('fs');
const p = 'D:/projects/CodeWorld/.cc-wt-d018/bin/codeworld/src/main.rs';
let s = fs.readFileSync(p, 'utf8');
const anchor = '            saw_projectile |= world.entities.values().any(|e| is_creation_shell(e) && e.is_alive());';
if (!s.includes(anchor)) throw new Error('anchor not found');
const diag = anchor + '\r\n' + [
'            eprintln!(',
'                "DBG t={} entities={} shells={} markers={:?} H_alive={}",',
'                world.tick(),',
'                world.entities.len(),',
'                world.entities.values().filter(|e| is_creation_shell(e)).count(),',
'                map.collect_all_log_markers(),',
'                world.entities.get(&h).map(|e| e.is_alive()).unwrap_or(false),',
'            );',
].join('\r\n');
s = s.replace(anchor, diag);
fs.writeFileSync(p, s);
console.log('diag added');
