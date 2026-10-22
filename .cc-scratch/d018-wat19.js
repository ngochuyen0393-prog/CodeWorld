// D-018：三个新 WAT 生成器同步 D-009 的 19 参 spawn（签名补 2 个 f64 + 压栈尾部补 2 个液体参）
const fs = require('fs');
const p = 'D:/projects/CodeWorld/.cc-wt-d018/bin/codeworld/src/main.rs';
let L = fs.readFileSync(p, 'utf8').split('\n');
const sig17 = '(param i32 i32 f64 f64 i32 i32 f64 f64 i32 i32 f64 f64 i32 i32 f64 f64 f64)';
const sig19 = '(param i32 i32 f64 f64 i32 i32 f64 f64 i32 i32 f64 f64 i32 i32 f64 f64 f64 f64 f64)';
// 本刀三处生成器行号（1 基）：3728 hunter_wat / 8639 shooter_once_wat / 11159 dying_will_firework_wat
const targets = new Set([3728, 8639, 11159]);
let fixed = 0;
for (let i = 0; i < L.length; i++) {
    const ln = i + 1;
    if (targets.has(ln) && L[i].includes(sig17)) {
        L[i] = L[i].split(sig17).join(sig19);
        // 找本生成器的 spawn 调用尾部（f64.const 0.0)) 三连后）补两个 f64.const 0.0
        // 定位：从此行向下找 '(f64.const 0.0))))'（hunter）或 '(f64.const 0.0))))))'（shooter_once/会因 then 层数不同）
        // 更稳：找 'call $spawn' 压栈块结束 = 下一个以 'f64.const 0.0' 结尾的行后无更多 f64 参数行
        for (let k = i + 1; k < Math.min(i + 60, L.length); k++) {
            // spawn 压栈块的最后一个参数行：17 参的最后一个 field_drag_delta=0.0
            if (L[k].includes('(f64.const 0.0))') && L[k].includes('f64.const 0.0)') && !L[k].includes('liquid')) {
                // 确认是 spawn 压栈尾部（前面 3 行内出现过 'call $spawn' 或其参数）
                let isSpawnBlock = false;
                for (let j = Math.max(i, k - 25); j < k; j++) {
                    if (L[j].includes('call $spawn')) { isSpawnBlock = true; break; }
                }
                if (!isSpawnBlock) continue;
                // 17 参尾行：在最后两个 '))' 前插入两个液体 f64.const
                // 该行形如 '          (f64.const 0.0)))' 或 '...(f64.const 0.0))))'
                const prefix = L[k].slice(0, L[k].indexOf('(f64.const 0.0))'));
                const rest = L[k].slice(L[k].indexOf('(f64.const 0.0))'));
                // rest 以 '(f64.const 0.0)' 开头，其后是 N 个 ')'
                const closes = rest.slice('(f64.const 0.0)'.length);
                L[k] = prefix + '(f64.const 0.0)\n          (f64.const 0.0)\n          (f64.const 0.0)' + closes;
                fixed++;
                break;
            }
        }
    }
}
fs.writeFileSync(p, L.join('\n'));
console.log('spawn sigs upgraded to 19 params:', fixed);
