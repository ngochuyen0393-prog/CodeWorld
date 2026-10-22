// D-018 诊断 v3：shell_wat_escaped 各阶段可编译性
const fs = require('fs');
const p = 'D:/projects/CodeWorld/.cc-wt-d018/bin/codeworld/src/main.rs';
let s = fs.readFileSync(p, 'utf8');
const anchorName = 'fn smoke_ballistics_launch_to_intercept_evidence_chain() {';
const idx = s.indexOf(anchorName);
const tstart = s.lastIndexOf('#[test]', idx);
const lineStart = s.lastIndexOf('\n', tstart) + 1;
const diag = [
'    #[test]',
'    fn diag_shell_code_truncation() {',
'        let esc = shell_wat_escaped();',
'        println!("raw len={} esc len={}", CTRL_SHELL_WAT.len(), esc.len());',
'        let shooter = shooter_once_wat();',
'        println!("shooter wat parse={}", wat::parse_str(&shooter).is_ok());',
'        println!("esc parse={}", wat::parse_str(&esc).is_ok());',
'        // 直接看 shooter WAT 里 data 段的前 120 字符',
'        let i = shooter.find("(data (i32.const 0)").unwrap();',
'        println!("data head: {}", &shooter[i..i + 120].replace(chr(10), "|"))',
'    }',
'',
'    ',
].join('\r\n').replace('chr(10)', String.fromCharCode(39) + String.fromCharCode(92) + 'n' + String.fromCharCode(39));
s = s.slice(0, lineStart) + diag + s.slice(lineStart);
fs.writeFileSync(p, s);
console.log('diag v3 inserted');
