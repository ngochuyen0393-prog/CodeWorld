// D-018：重建 swept_first_body_contact 函数（清除 step_projectiles 交错残骸）
// 当前 L179-347 是交错残骸；正确的 swept 函数 = 注释(L168-178) + 干净实现
const fs = require('fs');
const p = 'D:/projects/CodeWorld/.cc-wt-d018/crates/physics/src/default.rs';
let L = fs.readFileSync(p, 'utf8').split('\n');
// 定位：签名行（含 'fn swept_first_body_contact'）
const sigIdx = L.findIndex(l => l.includes('fn swept_first_body_contact'));
if (sigIdx < 0) throw new Error('sig not found');
// 函数结束：从 sigIdx 向后找 '        best' 行，其后一行应为 '    }'
let bestIdx = -1;
for (let k = sigIdx; k < L.length; k++) {
    if (L[k].trim() === 'best') { bestIdx = k; break; }
}
if (bestIdx < 0) throw new Error('best not found');
const closeIdx = bestIdx + 1; // '    }'
// 替换 sigIdx..closeIdx（0 基含）为干净实现
const clean = [
'    fn swept_first_body_contact(',
'        &self,',
'        world: &WorldState,',
'        me_id: EntityId,',
'        from: (f64, f64),',
'        to: (f64, f64),',
'        me_r: f64,',
'    ) -> Option<(EntityId, f64)> {',
'        let mut others: Vec<EntityId> = world',
'            .entities',
'            .iter()',
'            .filter(|(oid, e)| {',
'                e.is_alive()',
'                    && **oid != me_id',
'                    && !codeworld_core::collision::is_projectile(e)',
'            })',
'            .map(|(oid, _)| *oid)',
'            .collect();',
'        others.sort_by_key(|oid| oid.0); // 确定序：t 并列时先扫到者保留（id 升序）',
'        let mut best: Option<(EntityId, f64)> = None;',
'        for oid in others {',
'            let Some(target) = world.entities.get(&oid) else {',
'                continue;',
'            };',
'            let contact_t = match body_of(target) {',
'                Body::Circle { center, radius: tr } => {',
'                    codeworld_core::collision::segment_first_contact_circle(',
'                        from, to, center, me_r + tr,',
'                    )',
'                }',
'                Body::Aabb { min, max } => {',
'                    codeworld_core::collision::segment_first_contact_expanded_aabb(',
'                        from, to, min, max, me_r,',
'                    )',
'                }',
'            };',
'            if let Some(t) = contact_t {',
'                if best.as_ref().is_none_or(|(_, bt)| t < *bt) {',
'                    best = Some((oid, t));',
'                }',
'            }',
'        }',
'        best',
'    }',
];
L.splice(sigIdx, closeIdx - sigIdx + 1, ...clean);
fs.writeFileSync(p, L.join('\r\n'));
console.log('swept function rebuilt at line', sigIdx + 1);
