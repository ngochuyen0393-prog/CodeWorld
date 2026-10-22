// D-018 诊断：截断 450 字节后的转义文本长什么样
const fs = require('fs');
const s = fs.readFileSync('D:/projects/CodeWorld/.cc-wt-d018/bin/codeworld/src/main.rs', 'utf8');
const m = s.match(/const CTRL_SHELL_WAT: &str = r#"(.*?)"#;/s);
const raw = m[1];
const escaped = raw.replace(/"/g, '\\"').replace(/\n/g, '\\0a');
const cut = escaped.slice(0, 450);
console.log('cut tail 80:', JSON.stringify(cut.slice(-80)));
console.log('=== raw tail 60 ===');
console.log(JSON.stringify(raw.slice(-60)));
