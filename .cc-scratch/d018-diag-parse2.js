// D-018 诊断 v2：CRLF 兼容
const fs = require('fs');
const p = 'D:/projects/CodeWorld/.cc-wt-d018/bin/codeworld/src/main.rs';
let s = fs.readFileSync(p, 'utf8');
const anchorName = 'fn smoke_ballistics_launch_to_intercept_evidence_chain() {';
const idx = s.indexOf(anchorName);
if (idx < 0) throw new Error('anchor not found');
// 回溯到该测试的 #[test] 行
const tstart = s.lastIndexOf('#[test]', idx);
const lineStart = s.lastIndexOf('\n', tstart) + 1;
const diag = [
'    #[test]',
'    fn diag_shell_code_truncation() {',
'        let esc = shell_wat_escaped();',
'        println!("raw len={} escaped len={}", CTRL_SHELL_WAT.len(), esc.len());',
'        let cut = &esc[..CTRL_SHELL_WAT.len()];',
'        let tail = cut.char_indices().rev().nth(60).map(|(i,_)| i).unwrap_or(0);',
'        println!("cut tail: {:?}", &cut[tail..]);',
'        println!("cut parse ok={}", wat::parse_str(cut).is_ok());',
'        println!("full parse ok={}", wat::parse_str(&esc).is_ok());',
'    }',
'',
'    ',
].join('\r\n');
s = s.slice(0, lineStart) + diag + s.slice(lineStart);
fs.writeFileSync(p, s);
console.log('diag inserted at char', lineStart);
