// D-018：smoke 逐拍打印（每次 tick 后插一行）——LF/CRLF 混合行尾安全版
const fs = require('fs');
const p = 'D:/projects/CodeWorld/.cc-wt-d018/bin/codeworld/src/main.rs';
let L = fs.readFileSync(p, 'utf8').split('\n');
const marker = 'fn smoke_ballistics_launch_to_intercept_evidence_chain() {';
const i0 = L.findIndex(l => l.includes(marker));
const iEnd = L.findIndex((l, i) => i > i0 && l.trim() === '    #[test]');
const tickLine = '        tick(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map);';
let count = 0;
for (let i = i0; i < iEnd; i++) {
    if (L[i].trim() === tickLine.trim()) {
        count++;
        const probe = [
'        if let Some(sh) = world.entities.values().find(|e| is_creation_shell(e)) {',
'            eprintln!("TK n=' + count + ' pos={:?} vel={:?} alive={} age={} fired={}", sh.pos(), sh.velocity(), sh.is_alive(), call_i32_export(&mut map, &sh.id, "get_age"), call_i32_export(&mut map, &sh.id, "get_fired"));',
'        } else {',
'            eprintln!("TK n=' + count + ' shell gone");',
'        }',
        ];
        L.splice(i + 1, 0, ...probe);
        i += probe.length;
    }
}
fs.writeFileSync(p, L.join('\n'));
console.log('probes added after', count, 'ticks');
