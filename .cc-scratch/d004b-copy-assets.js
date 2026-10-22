// D-004b 资产拷贝脚本：上游 ToonShading（只读）-> bin/codeworld/static/
// 拷：模型 glb、9 张贴图、basis/draco 解码器；顺带检测 glb 是否含 draco 压缩
const fs = require('fs');
const path = require('path');

const UP = 'C:/Users/Administrator/AppData/Local/hermes/kanban/boards/codeworld/workspaces/t_e8d28006/ToonShading';
const DST = 'D:/projects/CodeWorld/bin/codeworld/static';

function copy(src, dst) {
  fs.mkdirSync(path.dirname(dst), { recursive: true });
  fs.copyFileSync(src, dst);
  console.log('copied', dst, fs.statSync(dst).size, 'bytes');
}

// 模型
copy(`${UP}/static/models/ayakaktx2opt.glb`, `${DST}/models/ayakaktx2opt.glb`);

// 贴图（bg/ 不拷）
for (const dir of ['body', 'face', 'hair', 'matcap']) {
  for (const f of fs.readdirSync(`${UP}/static/textures/${dir}`)) {
    copy(`${UP}/static/textures/${dir}/${f}`, `${DST}/textures/${dir}/${f}`);
  }
}

// 解码器（basis / draco）
for (const lib of ['basis', 'draco']) {
  for (const f of fs.readdirSync(`${UP}/public/libs/${lib}`)) {
    copy(`${UP}/public/libs/${lib}/${f}`, `${DST}/vendor/${lib}/${f}`);
  }
}

// draco 扩展检测：扫 glb 二进制找 KHR_draco_mesh_compression 字符串
const glb = fs.readFileSync(`${UP}/static/models/ayakaktx2opt.glb`);
const has = glb.includes(Buffer.from('KHR_draco_mesh_compression'));
console.log('GLB contains KHR_draco_mesh_compression:', has);

// KTX2 检测（_basis 形式纹理）
const hasKtx = glb.includes(Buffer.from('KHR_texture_basisu')) || glb.includes(Buffer.from('\x01\x00\x00\x00KTX 20'));
console.log('GLB contains KHR_texture_basisu:', glb.includes(Buffer.from('KHR_texture_basisu')));
