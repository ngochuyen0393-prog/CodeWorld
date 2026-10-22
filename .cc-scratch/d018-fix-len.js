// D-018：shell_len 改用转义后文本长度（三处）+ 删诊断测试
const fs = require('fs');
const p = 'D:/projects/CodeWorld/.cc-wt-d018/bin/codeworld/src/main.rs';
let s = fs.readFileSync(p, 'utf8');
const nBefore = (s.match(/shell_len = CTRL_SHELL_WAT\.len\(\)/g) || []).length;
s = s.split('shell_len = CTRL_SHELL_WAT.len()').join('shell_len = shell_wat_escaped().len()');
// 删诊断测试
const d0 = s.indexOf('    fn diag_shell_code_truncation() {');
if (d0 >= 0) {
    const t0 = s.lastIndexOf('    #[test]', d0);
    let dEnd = s.indexOf('    }', d0) + '    }'.length;
    // 吃掉后面的空行
    while (s[dEnd] === '\r' || s[dEnd] === '\n') dEnd++;
    s = s.slice(0, t0) + s.slice(dEnd);
    console.log('diag test removed');
}
fs.writeFileSync(p, s);
console.log('shell_len refs fixed:', nBefore);
