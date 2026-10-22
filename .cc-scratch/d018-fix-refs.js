// D-018：把三处内联转义换成 shell_wat_escaped() 调用
const fs = require('fs');
const p = 'D:/projects/CodeWorld/.cc-wt-d018/bin/codeworld/src/main.rs';
let s = fs.readFileSync(p, 'utf8');
// 内联写法（Rust 源文本）：CTRL_SHELL_WAT.replace('"', "\")
const inline = `CTRL_SHELL_WAT.replace('"', "\\\\")` ; // 探测用
const needle = 'CTRL_SHELL_WAT.replace(\'"\', "\\\\"")';
const n = (s.match(/CTRL_SHELL_WAT\n?(\s*)\.replace\('"', "\\\\\\\\""\)/g) || []).length;
console.log('pattern A count:', n);
// 更宽松：所有 "CTRL_SHELL_WAT.replace" 出现
const all = (s.match(/CTRL_SHELL_WAT\s*\.replace/g) || []).length;
console.log('all replace refs:', all);
let count = 0;
s = s.replace(/CTRL_SHELL_WAT\s*\n?\s*\.replace\('", "[^"]*"\)/g, 'SHELLX');
fs.writeFileSync(p, s);
