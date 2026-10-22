// D-018：smoke 逐拍 age 追踪
const fs = require('fs');
const p = 'D:/projects/CodeWorld/.cc-wt-d018/bin/codeworld/src/main.rs';
let s = fs.readFileSync(p, 'utf8');
// 把 smoke 的 4 次 tick 循环替换为带打印版本
const oldLoop = '        // tick1：spawn 排队 -> 造物弹诞生 -> 第一拍飞行\r\n        tick(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map);';
if (!s.includes(oldLoop)) throw new Error('loop anchor not found');
const newLoop = [
'        // tick1：spawn 排队 -> 造物弹诞生 -> 第一拍飞行',
'        tick(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map);',
'        fn dbg_age(map: &EntitySandboxMap, world: &WorldState, tag: &str) {',
'            if let Some(sh) = world.entities.values().find(|e| is_creation_shell(e)) {',
'                eprintln!,',
'                    "AGE {}: alive={} age={} fired={}",',
'                    tag,',
'                    sh.is_alive(),',
'                    call_i32_export_pub(map, &sh.id, "get_age"),',
'                    call_i32_export_pub(map, &sh.id, "get_fired"),',
'                );',
'            } else {',
'                eprintln!("AGE {}: shell gone", tag);',
'            }',
'        }',
'        dbg_age(&map, &world, "t1");',
].join('\r\n');
s = s.replace(oldLoop, newLoop);
fs.writeFileSync(p, s);
console.log('per-tick age trace added');
