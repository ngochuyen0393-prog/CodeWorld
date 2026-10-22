// D-018：数教学文档 data[4096] 解码后字节数（spawn code_len 应填的值）
const fs = require('fs');
const s = fs.readFileSync('D:/projects/CodeWorld/.cc-wt-d018/docs/newbie-first-thrower.md', 'utf8');
const m = s.match(/\(data \(i32\.const 4096\) "(.*?)"\)/s);
if (!m) { console.log('data not found'); process.exit(0); }
const txt = m[1];
// 文档里是 WAT 源码转义形式：\0a（源码 4 字符→1 字节）与 \"（2→1）；\00（3→1）
let bytes = 0, i = 0;
while (i < txt.length) {
    if (txt[i] === '\\') {
        const rest = txt.slice(i);
        if (rest.startsWith('\\0a')) { bytes++; i += 4; }
        else if (rest.startsWith('\\00')) { bytes++; i += 4; }
        else if (rest.startsWith('\\"')) { bytes++; i += 2; }
        else if (rest.startsWith('\\\\')) { bytes++; i += 2; }
        else { bytes++; i++; }
    } else { bytes++; i++; }
}
console.log('decoded bytes:', bytes);
