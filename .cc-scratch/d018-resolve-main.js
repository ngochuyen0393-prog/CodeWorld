// D-018：解 main.rs 两处冲突（按行区间处理，避免转义地狱）
const fs = require('fs');
const p = 'D:/projects/CodeWorld/.cc-wt-d018/bin/codeworld/src/main.rs';
let L = fs.readFileSync(p, 'utf8').split('\n');
let resolved = 0;
for (let pass = 0; pass < 2; pass++) {
    let i = -1;
    for (let k = 0; k < L.length; k++) {
        if (L[k].startsWith('<<<<<<< HEAD')) { i = k; break; }
    }
    if (i < 0) break;
    let m = -1, e = -1;
    for (let k = i + 1; k < L.length; k++) {
        if (L[k].startsWith('=======') && m < 0) m = k;
        if (L[k].startsWith('>>>>>>> a1804fc')) { e = k; break; }
    }
    if (m < 0 || e < 0) throw new Error('marker structure broken at line ' + (i + 1));
    // 冲突1（第一个）：HEAD 侧含 liquids 收集段要保留 + a1804fc 侧的 a1.45 注释
    // 冲突2（第二个）：保 a1804fc 侧（退役注记）
    const headSide = L.slice(i + 1, m);
    const mySide = L.slice(m + 1, e);
    let merged;
    if (resolved === 0) {
        // 冲突1：HEAD 的 liquids 收集代码行保留（let liquids/tracing/if 块），
        // 但注释更新为 D-018 口径；a1.45 注释用我的版本
        merged = [
            '    //   a1.42 液体域清单收集（D-009 登记制刷场，照 fields 先例）：每拍全量重算',
            '    //       覆盖关系（无 enter/exit 缓存），alive+liquid_density>0 的格对齐 AABB',
            '    //       即水域。产物喂物理步（【D-018】弹道已并入 c 步冲量路径）——数据流',
            '    //       两端日志此为收集端，结算端在 physics step 内（实体在水中 INFO 带 f 值）。',
            ...headSide.filter(l => l.includes('let liquids =') || l.includes('tracing::info!(tick = current_tick, count = liquids.len(), "[liquid]') || l.trim() === '}' || l.trim() === ''),
            ...mySide,
        ];
    } else {
        merged = mySide;
    }
    L.splice(i, e - i + 1, ...merged);
    resolved++;
}
fs.writeFileSync(p, L.join('\n'));
console.log('resolved', resolved, 'conflicts; markers left:',
    L.filter(l => l.startsWith('<<<<<<<') || l.startsWith('>>>>>>>')).length);
