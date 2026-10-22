// D-018：木浮铁沉逐拍诊断（临时测试）
const fs = require('fs');
const p = 'D:/projects/CodeWorld/.cc-wt-d018/bin/codeworld/src/main.rs';
let s = fs.readFileSync(p, 'utf8');
const anchor = '        let wood_y = world.entities.get(&wood_id).unwrap().pos().1;';
if (!s.includes(anchor)) throw new Error('anchor');
const diag = [
'        for t2 in 0..20 {',
'            let liquids2 = codeworld_physics::liquid::collect_liquid_domains(&world);',
'            physics.step_with_liquids(&mut world, &aloft, &[], &liquids2);',
'            let wy = world.entities.get(&wood_id).unwrap().pos().1;',
'            let wv = world.entities.get(&wood_id).unwrap().velocity().1;',
'            let iy = world.entities.get(&iron_id).unwrap().pos().1;',
'            eprintln!("FLT t={t2} wood_y={wy:.4} wood_vy={wv:.4} iron_y={iy:.4}");',
'        }',
].join('\r\n');
s = s.replace(anchor, diag + '\r\n' + anchor);
fs.writeFileSync(p, s);
console.log('float diag added');
