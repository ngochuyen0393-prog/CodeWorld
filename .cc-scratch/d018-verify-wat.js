// D-018：验证 hunter/shooter 压栈尾部液体参
const fs = require('fs');
const L = fs.readFileSync('D:/projects/CodeWorld/.cc-wt-d018/bin/codeworld/src/main.rs', 'utf8').split('\n');
for (const ln of [3728, 8639]) {
    let end = -1;
    for (let k = ln; k < ln + 60; k++) {
        if (L[k - 1].includes('call $spawn')) { end = k; break; }
    }
    if (end < 0) { console.log('gen@' + ln + ': no spawn call found'); continue; }
    for (let k = end; k < end + 30; k++) {
        if (L[k - 1].includes('(f64.const 0.0)') && L[k - 1].includes('))')) {
            console.log('gen@' + ln + ' tail line ' + k + ':', L[k - 1].trim().slice(0, 80));
            for (let j = k - 4; j <= k - 2; j++) {
                console.log('   ' + j + ':', L[j - 1] ? L[j - 1].trim() : '');
            }
            break;
        }
    }
}
