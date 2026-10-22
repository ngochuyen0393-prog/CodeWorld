// D-004a 验收辅助：提取 index.html 两段内联脚本，分别落盘做语法检查
var fs = require('fs');
var html = fs.readFileSync('bin/codeworld/static/index.html', 'utf8');

var classicAnchor = '<script>'; // 经典脚本标签无属性，module 标签含 type 属性不会误中
var moduleAnchor = '<script type="module">';

var ci = html.indexOf(classicAnchor);
var mi = html.lastIndexOf(moduleAnchor); // 头部注释里也提到该标签字样，取最后一处=真标签
if (ci < 0) { console.error('FAIL: 经典脚本锚点未找到'); process.exit(1); }
if (mi < 0) { console.error('FAIL: module 脚本锚点未找到'); process.exit(1); }

var classicBody = html.slice(ci + '<script>'.length, html.indexOf('</script>', ci));
var moduleBody = html.slice(mi + moduleAnchor.length, html.indexOf('</script>', mi));

fs.writeFileSync('.cc-scratch/d004a-check-classic.js', classicBody);
fs.writeFileSync('.cc-scratch/d004a-check-module.mjs', moduleBody);
console.log('classic 脚本长度=' + classicBody.length);
console.log('module 脚本长度=' + moduleBody.length);
