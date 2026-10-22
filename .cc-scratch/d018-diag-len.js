// D-018 诊断：CTRL_SHELL_WAT 原文长度 vs 转义后长度
const fs = require('fs');
const s = fs.readFileSync('D:/projects/CodeWorld/.cc-wt-d018/bin/codeworld/src/main.rs', 'utf8');
const m = s.match(/const CTRL_SHELL_WAT: &str = r#"(.*?)"#;/s);
if (!m) throw new Error('CTRL_SHELL_WAT not found');
const raw = m[1];
const escaped = raw.replace(/"/g, '\\"').replace(/\n/g, '\\0a');
console.log('raw len:', raw.length);
console.log('escaped len:', escaped.length);
console.log('delta:', escaped.length - raw.length);
