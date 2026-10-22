// D-018：删 hunter_shoots 诊断 v2
const fs = require('fs');
const p = 'D:/projects/CodeWorld/.cc-wt-d018/bin/codeworld/src/main.rs';
let s = fs.readFileSync(p, 'utf8');
const anchor = '            saw_projectile |= world.entities.values().any(|e| is_creation_shell(e) && e.is_alive());';
const i = s.indexOf(anchor);
if (i < 0) throw new Error('anchor not found');
let end = s.indexOf('            );', i);
if (end < 0) throw new Error('diag end not found');
end = s.indexOf('\n', end) + 1;
s = s.slice(0, i + anchor.length) + '\r\n' + s.slice(end);
fs.writeFileSync(p, s);
console.log('diag v2 removed');
