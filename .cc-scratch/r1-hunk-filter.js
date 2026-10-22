// R1 选择性提交助手：从 git diff 输出中按内容标记挑出本卡 hunks。
// 用法：node r1-hunk-filter.js <diff文件> <标记正则1>[,<标记正则2>...]  （标记命中文体任一行即收该 hunk）
// 输出到 stdout。文件头（diff/index/---/+++）恒带。
const fs = require('fs');
const [file, markersArg] = process.argv.slice(2);
const markers = markersArg.split(',').map((s) => new RegExp(s));
const lines = fs.readFileSync(file, 'utf8').split('\n');
let header = '';
const hunks = [];
let cur = null;
for (const line of lines) {
  if (/^(diff --git|index |--- |\+\+\+ )/.test(line)) { header += line + '\n'; continue; }
  if (/^@@/.test(line)) { cur = { head: line + '\n', body: '', keep: false }; hunks.push(cur); continue; }
  if (cur) {
    cur.body += line + '\n';
    if (markers.some((m) => m.test(line))) cur.keep = true;
  }
}
process.stdout.write(header);
for (const h of hunks) if (h.keep) process.stdout.write(h.head + h.body);
