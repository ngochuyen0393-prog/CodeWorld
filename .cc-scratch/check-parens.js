// field-demo.wat 括号配平逐行检查（临时诊断脚本，.cc-scratch 工件）
const fs = require('fs');
const lines = fs.readFileSync('examples/field-demo.wat', 'utf8').split('\n');
let d = 0, inStr = false;
lines.forEach((l, i) => {
  let inCmt = false;
  for (let j = 0; j < l.length; j++) {
    const c = l[j];
    if (inCmt) break;
    if (inStr) {
      if (c === '\\') { j++; } else if (c === '"') inStr = false;
      continue;
    }
    if (c === ';' && l[j + 1] === ';') { inCmt = true; break; }
    if (c === '"') { inStr = true; continue; }
    if (c === '(') d++;
    else if (c === ')') d--;
  }
  if (i + 1 >= 78) console.log(i + 1, 'd=' + d, JSON.stringify(l.slice(0, 46)));
});
console.log('end', d);
