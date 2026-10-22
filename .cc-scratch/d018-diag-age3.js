// D-018：smoke 每次 tick 后打 age（利用现有 call_i32_export）
const fs = require('fs');
const p = 'D:/projects/CodeWorld/.cc-wt-d018/bin/codeworld/src/main.rs';
let s = fs.readFileSync(p, 'utf8');
// 找 smoke 测试里的 4 个独立 tick 调用块（tick(&mut world, ...) 各行后面插 age 打印）
const marker = 'fn smoke_ballistics_launch_to_intercept_evidence_chain() {';
const i0 = s.indexOf(marker);
const iEnd = s.indexOf('    #[test]', i0 + 10);
let seg = s.slice(i0, iEnd);
const tickCall = 'tick(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map);';
let n = 0;
seg = seg.split(tickCall).join(tickCall + `
        if let Some(sh) = world.entities.values().find(|e| is_creation_shell(e)) {
            eprintln!(
                "TICKAGE alive={} age={} fired={}",
                sh.is_alive(),
                call_i32_export(&mut map, &sh.id, "get_age"),
                call_i32_export(&mut map, &sh.id, "get_fired")
            );
        } else {
            eprintln!("TICKAGE shell gone");
        }`);
// 会被替换 N 次（4 次 tick）
s = s.slice(0, i0) + seg + s.slice(iEnd);
fs.writeFileSync(p, s);
console.log('tick-age trace added after', (seg.match(/TICKAGE/g)||[]).length, 'ticks');
