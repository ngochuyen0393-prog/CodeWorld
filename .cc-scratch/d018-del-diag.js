// D-018：删诊断测试
const fs = require('fs');
const p = 'D:/projects/CodeWorld/.cc-wt-d018/bin/codeworld/src/main.rs';
let s = fs.readFileSync(p, 'utf8');
const d0 = s.indexOf('    fn diag_shell_code_truncation() {');
if (d0 >= 0) {
    const t0 = s.lastIndexOf('    #[test]', d0);
    let dEnd = s.indexOf('    }', d0) + '    }'.length;
    while (s[dEnd] === '\r' || s[dEnd] === '\n') dEnd++;
    s = s.slice(0, t0) + s.slice(dEnd);
    console.log('diag removed');
} else {
    console.log('no diag found');
}
fs.writeFileSync(p, s);
