// D-018：删 smoke 里的 TK 探针（4 个块）
const fs = require('fs');
const p = 'D:/projects/CodeWorld/.cc-wt-d018/bin/codeworld/src/main.rs';
let L = fs.readFileSync(p, 'utf8').split('\n');
let removed = 0;
for (let i = L.length - 1; i >= 0; i--) {
    if (L[i].includes('eprintln!("TK n=')) {
        // 块结构：上一行='if let Some(sh) ... {'，下两行='} else {'、'eprintln!("TK n=..shell gone");'、'}'
        const start = i - 1;
        let end = i + 3; // '} else {' 在 i+1, gone 在 i+2, '}' 在 i+3
        if (L[end] && L[end].trim() === '}') {
            L.splice(start, end - start + 1);
            removed++;
        }
    }
}
fs.writeFileSync(p, L.join('\n'));
console.log('probes removed:', removed);
