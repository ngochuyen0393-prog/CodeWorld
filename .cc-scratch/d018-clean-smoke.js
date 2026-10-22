// D-018：清 smoke 全部诊断（TICKAGE 块 / SMOKE after 块 / SMOKE diag 块）
const fs = require('fs');
const p = 'D:/projects/CodeWorld/.cc-wt-d018/bin/codeworld/src/main.rs';
let s = fs.readFileSync(p, 'utf8');
function delBlock(startMarker, endMarker) {
    let n = 0;
    let i;
    while ((i = s.indexOf(startMarker)) >= 0) {
        const end = s.indexOf(endMarker, i);
        if (end < 0) break;
        const lineEnd = s.indexOf('\n', end) + 1;
        s = s.slice(0, i) + s.slice(lineEnd);
        n++;
    }
    return n;
}
// TICKAGE 块（if let Some(sh)...eprintln!("TICKAGE...} else {...}"）——按 else 分支尾删
let n1 = 0;
while (s.includes('eprintln!(\r\n                "TICKAGE') || s.includes('eprintln!("TICKAGE')) {
    const i = s.indexOf('if let Some(sh) = world.entities.values().find(|e| is_creation_shell(e)) {\r\n            eprintln!(');
    if (i < 0) break;
    // 找到这个块的结束：'} else {\r\n                eprintln!("TICKAGE shell gone");\r\n            }'
    const tailMarker = 'eprintln!("TICKAGE shell gone");';
    const t = s.indexOf(tailMarker, i);
    if (t < 0) break;
    let end = s.indexOf('}', s.indexOf('};', t)) ;
    // 简化：找 '            }\r\n' 在 tailMarker 之后第一次出现
    const after = s.indexOf(tailMarker, i);
    let close = s.indexOf('}', after);
    close = s.indexOf('\n', close) + 1;
    s = s.slice(0, i) + s.slice(close);
    n1++;
}
console.log('TICKAGE blocks removed:', n1);
// SMOKE after/diag 块：'        {\r\n            let sh = world.entities.values().find(|e| is_creation_shell(e));\r\n            eprintln!("SMOKE
let n2 = 0;
const smokeStart = '        {\r\n            let sh = world.entities.values().find(|e| is_creation_shell(e));\r\n            eprintln!("SMOKE';
while (s.includes(smokeStart)) {
    const i = s.indexOf(smokeStart);
    // 块结束：'        }\r\n'
    const close = s.indexOf('        }\r\n', i);
    if (close < 0) break;
    s = s.slice(0, i) + s.slice(close + '        }\r\n'.length);
    n2++;
}
console.log('SMOKE blocks removed:', n2);
// SMOKE diag（if let Some(sh) ... "SMOKE diag）
let n3 = 0;
const diagStart = '                if let Some(sh) = world.entities.values().find(|e| is_creation_shell(e)) {\r\n                    eprintln!("SMOKE diag';
while (s.includes(diagStart)) {
    const i = s.indexOf(diagStart);
    const close = s.indexOf('                }\r\n', i);
    if (close < 0) break;
    s = s.slice(0, i) + s.slice(close + '                }\r\n'.length);
    n3++;
}
console.log('SMOKE diag removed:', n3);
fs.writeFileSync(p, s);
