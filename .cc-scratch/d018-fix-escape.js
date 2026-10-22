// D-018 修复 shell_wat_escaped 辅助函数（bash 转义地狱的替代方案）
const fs = require('fs');
const p = 'D:/projects/CodeWorld/.cc-wt-d018/bin/codeworld/src/main.rs';
let L = fs.readFileSync(p, 'utf8').split('\n');
const iFn = L.findIndex(l => l.startsWith('fn shell_wat_escaped()'));
if (iFn < 0) throw new Error('fn not found');
// 回溯注释起点（连续 /// 行）
let cstart = iFn - 1;
while (cstart > 0 && L[cstart - 1].startsWith('///')) cstart--;
// 前向找函数结束（第一个单独 }）
let cend = iFn;
while (cend < L.length && L[cend].trim() !== '}') cend++;
// 转义目标代码（JS 字面量写 Rust 源）：
const fixed = [
    '/// CTRL_SHELL_WAT 内嵌转义（投掷器 data 段用）：引号转 \\" 、换行转 \\0a',
    '///（WAT 字符串禁裸换行——arena 测试 wat_escape 同款规则的最小版）',
    'fn shell_wat_escaped() -> String {',
    "    CTRL_SHELL_WAT",
    `        .replace('"', "\\\\"")`,
    `        .replace('\\n', "\\\\0a")`,
    '}',
];
L.splice(cstart, cend - cstart + 1, ...fixed);
fs.writeFileSync(p, L.join('\n'));
console.log('fixed helper at line', cstart + 1);
console.log(fixed.join('\n'));
