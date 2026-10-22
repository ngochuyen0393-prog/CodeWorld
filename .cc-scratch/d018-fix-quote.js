// D-018：修 2579 行的引号转义（文件字节精确控制）
const fs = require('fs');
const p = 'D:/projects/CodeWorld/.cc-wt-d018/bin/codeworld/src/main.rs';
let L = fs.readFileSync(p, 'utf8').split('\n');
// 目标 Rust 源：        .replace('"', "\\\"")
// 即：空格x8 + .replace(' + 单引号 + " + 单引号 + 逗号空格 + 双引号 + 反斜杠x2 + \" + 双引号 + )
const want = '        .replace(' + "'" + '"' + "'" + ', ' + '"\\\\\\""'.replace('\\\\','\\\\') + ')';
// 直接逐字符构造，避免再绕一层：
const line = '        .replace(' + "'" + '"' + "'" + ', "' + '\\\\' + '\\"' + '")';
console.log('constructed:', JSON.stringify(line));
L[2578] = line;
fs.writeFileSync(p, L.join('\n'));
console.log('written');
