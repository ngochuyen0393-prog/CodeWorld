// D-018-R1：投掷器教学模板子弹代码字节数实测脚本
//
// 用途：从 docs/newbie-first-thrower.md 的 data 段转义文本算出 spawn 第 2 参
//       code_len 的准确值（文档此前写死 661，与实测不符——照抄必死）。
//
// 【WAT data 段转义规则】（本脚本算法依据，wasmtime/wat 同款语义）：
//   1. data 段字符串是**字节串**：源码里的转义序列在模块实例化时解码成真实
//      字节写进线性内存，spawn 从内存读 [code_ptr, code_ptr+code_len) 拿到的
//      是**解码后**的文本——所以 code_len = 解码后字节数，不是转义文本字面长度。
//   2. 转义序列（按 WAT 规范逐字面识别、各解码为 1 字节）：
//        \"  -> 0x22 引号        \\  -> 0x5C 反斜杠     \' -> 0x27 单引号
//        \t  -> 0x09 制表        \n -> 0x0A 换行        \r -> 0x0D 回车
//        \hh -> 十六进制字节（恰好两位 hex，如 \0a=换行、\00=NUL）
//   3. 结尾 \00 是 NUL 哨兵（防御读越界），**不计入 code_len**——世界 NPC
//      同款口径：bin/codeworld/src/main.rs CTRL_SHELL_WAT.len()（原文长度，
//      不含哨兵）。超长（把哨兵算进去）= 源码尾部多一个 NUL 字节，wat 解析
//      同样会炸；截断（数少了）= 表达式被拦腰斩断，报 unknown operator。
//   4. 字符串内的裸换行/裸 CR 在 WAT 文本格式里合法（按原字节计）；本模板
//      文本已全转义为 \0a，不含裸换行。
//
// 附带交叉验证：与本仓 main.rs 的 CTRL_SHELL_WAT（世界 H 搅局者的弹，同源
// 教学 shell）解码后逐字节比对——相等则口径互证。
//
// 运行：node .cc-scratch/d018-count-shell-len.js

const fs = require('fs');
const path = require('path');

const repo = path.resolve(__dirname, '..');

// WAT 字节串解码：把源码里的转义序列还原成真实字节，返回 Buffer
function decodeWatString(escaped) {
  const out = [];
  let i = 0;
  while (i < escaped.length) {
    const ch = escaped[i];
    if (ch !== '\\') {
      // 非 ASCII（不该出现在本模板）按 UTF-8 算多字节
      const bytes = Buffer.from(ch, 'utf8');
      for (const b of bytes) out.push(b);
      i += 1;
      continue;
    }
    const nxt = escaped[i + 1];
    if (nxt === '"') { out.push(0x22); i += 2; }
    else if (nxt === '\\') { out.push(0x5c); i += 2; }
    else if (nxt === "'") { out.push(0x27); i += 2; }
    else if (nxt === 't') { out.push(0x09); i += 2; }
    else if (nxt === 'n') { out.push(0x0a); i += 2; }
    else if (nxt === 'r') { out.push(0x0d); i += 2; }
    else if (/[0-9a-fA-F]/.test(nxt)) {
      // \hh：恰好两位十六进制
      const hex = escaped.slice(i + 1, i + 3);
      if (!/^[0-9a-fA-F]{2}$/.test(hex)) {
        throw new Error('非法 \\hh 转义（hex 不足两位）@ 偏移 ' + i);
      }
      out.push(parseInt(hex, 16));
      i += 3;
    } else {
      throw new Error('未知转义 \\' + (nxt ?? 'EOF') + ' @ 偏移 ' + i);
    }
  }
  return Buffer.from(out);
}

// 从 markdown 抽出 data 段 (i32.const 4096) "..." 的转义文本
function extractDocShell() {
  const md = fs.readFileSync(path.join(repo, 'docs', 'newbie-first-thrower.md'), 'utf8');
  const m = md.match(/\(data \(i32\.const 4096\) "((?:[^"\\]|\\.)*)"\)/);
  if (!m) throw new Error('文档里没找到 (data (i32.const 4096) "…") 段');
  return m[1];
}

// 从 main.rs 抽出 CTRL_SHELL_WAT（r#"…"# 原文），与文档 shell 交叉验证
function extractCtrlShell() {
  const rs = fs.readFileSync(path.join(repo, 'bin', 'codeworld', 'src', 'main.rs'), 'utf8');
  const m = rs.match(/const CTRL_SHELL_WAT: &str = r#"([\s\S]*?)"#;/);
  if (!m) throw new Error('main.rs 里没找到 CTRL_SHELL_WAT');
  return m[1].replace(/\r\n/g, '\n').replace(/\r/g, '\n');
}

const docEscaped = extractDocShell();
const decoded = decodeWatString(docEscaped);

// 哨兵检查：解码后最后一个字节必须是 NUL（\00 结尾规则）
const sentinelOk = decoded.length > 0 && decoded[decoded.length - 1] === 0;
// code_len = 去掉哨兵后的解码字节数（= 原文 WAT 源码长度）
const codeLen = sentinelOk ? decoded.length - 1 : decoded.length;

console.log('=== 投掷器模板子弹代码字节数实测 ===');
console.log('data 段转义文本字面长度 :', docEscaped.length, '（仅供参考——不是 code_len）');
console.log('解码后总字节（含哨兵）  :', decoded.length);
console.log('结尾 \\00 哨兵          :', sentinelOk ? '在（合法）' : '缺失（异常！）');
console.log('code_len 实测值         :', codeLen, '  ← 文档 spawn 第 2 参应填这个数');

// 交叉验证：与世界 NPC 同源 shell 比对
const ctrlRaw = extractCtrlShell();
const ctrlLen = Buffer.byteLength(ctrlRaw, 'utf8');
const docTextNoSentinel = decoded.slice(0, codeLen).toString('utf8');
console.log('---');
console.log('main.rs CTRL_SHELL_WAT 原文长度:', ctrlLen, '（世界 NPC 实际在用，活证口径）');
console.log('文档 shell 与 CTRL_SHELL_WAT  :', docTextNoSentinel === ctrlRaw ? '逐字节一致（互证通过）' : '不一致（文档是独立教学 shell，仅口径参照）');

// 与文档当前写的 661 对照
const md = fs.readFileSync(path.join(repo, 'docs', 'newbie-first-thrower.md'), 'utf8');
const written = md.match(/\(i32\.const (\d+)\)\s+;\s*2\s+code_len/);
if (written) {
  const w = parseInt(written[1], 10);
  console.log('---');
  console.log('文档当前写的 code_len  :', w, w === codeLen ? '（与实测一致）' : '（与实测不符！照抄会 ' + (w < codeLen ? '截断炸 unknown operator' : '超长多读哨兵') + '）');
}
