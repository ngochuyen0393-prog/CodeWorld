// D-018：三处内联转义 → shell_wat_escaped() 调用（行级精确替换）
const fs = require('fs');
const p = 'D:/projects/CodeWorld/.cc-wt-d018/bin/codeworld/src/main.rs';
let L = fs.readFileSync(p, 'utf8').split('\n');
let n = 0;
for (let i = 0; i < L.length; i++) {
    // Rust 源行： let shell_escaped = CTRL_SHELL_WAT.replace('"', "\");
    if (L[i].includes('let shell_escaped = CTRL_SHELL_WAT.replace')) {
        L[i] = '        let shell_escaped = shell_wat_escaped();';
        n++;
    }
}
fs.writeFileSync(p, L.join('\n'));
console.log('replaced', n, 'inline escapes');
