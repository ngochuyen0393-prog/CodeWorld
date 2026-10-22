// D-018 诊断：在 main.rs 测试区临时插入截断编译诊断测试
const fs = require('fs');
const p = 'D:/projects/CodeWorld/.cc-wt-d018/bin/codeworld/src/main.rs';
let s = fs.readFileSync(p, 'utf8');
const anchor = '    #[test]\n    fn smoke_ballistics_launch_to_intercept_evidence_chain() {';
if (!s.includes(anchor)) throw new Error('anchor not found');
const diag = `    #[test]
    fn diag_shell_code_truncation() {
        let esc = shell_wat_escaped();
        println!("raw len={} escaped len={}", CTRL_SHELL_WAT.len(), esc.len());
        let cut = &esc[..CTRL_SHELL_WAT.len()];
        println!("cut tail: {:?}", &cut[..cut.len().min(80)]);
        println!("cut parse ok={}", wat::parse_str(cut).is_ok());
        println!("full parse ok={}", wat::parse_str(&esc).is_ok());
    }

`;
s = s.replace(anchor, diag + anchor);
fs.writeFileSync(p, s);
console.log('diag test inserted');
