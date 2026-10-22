// D-004a 验收辅助：在真 r160 库上跑场景构建代码路径（与 index.html module 段同一套调用）
import * as THREE from './three-under-test.mjs';

var fail = [];
function check(name, cond) {
  console.log((cond ? 'PASS' : 'FAIL') + '  ' + name);
  if (!cond) fail.push(name);
}

// 用到的导出必须齐（防 API 名拼错/版本不存在）
check('THREE.REVISION = r' + THREE.REVISION, THREE.REVISION === '160');
check('WebGLRenderer 导出', typeof THREE.WebGLRenderer === 'function');
check('BoxGeometry 导出', typeof THREE.BoxGeometry === 'function');
check('MeshBasicMaterial 导出', typeof THREE.MeshBasicMaterial === 'function');
check('BufferAttribute 导出', typeof THREE.BufferAttribute === 'function');
check('AxesHelper 导出', typeof THREE.AxesHelper === 'function');
check('PerspectiveCamera 导出', typeof THREE.PerspectiveCamera === 'function');
check('Scene 导出', typeof THREE.Scene === 'function');
check('Mesh 导出', typeof THREE.Mesh === 'function');

// 场景构建路径（照抄 index.html openLayer 中非 DOM 部分）
var geo = new THREE.BoxGeometry(1, 1, 1);
var pos = geo.attributes.position;
var colors = new Float32Array(pos.count * 3);
for (var i = 0; i < pos.count; i++) {
  colors[i * 3]     = 0.3 + (pos.getX(i) + 0.5) * 0.7;
  colors[i * 3 + 1] = 0.3 + (pos.getY(i) + 0.5) * 0.7;
  colors[i * 3 + 2] = 0.3 + (pos.getZ(i) + 0.5) * 0.7;
}
geo.setAttribute('color', new THREE.BufferAttribute(colors, 3));
var mat = new THREE.MeshBasicMaterial({ wireframe: true, vertexColors: true });
var cube = new THREE.Mesh(geo, mat);
var scene = new THREE.Scene();
scene.add(cube);
scene.add(new THREE.AxesHelper(1.5));
var camera = new THREE.PerspectiveCamera(50, 16 / 9, 0.1, 100);
camera.position.set(2.4, 1.8, 2.6);
camera.lookAt(0, 0, 0);

check('BoxGeometry 顶点数=24（6面x4）', pos.count === 24);
check('顶点色分量域 [0.3,1.0]',
  Math.min.apply(null, Array.from(colors)) >= 0.3 - 1e-9 &&
  Math.max.apply(null, Array.from(colors)) <= 1.0 + 1e-9);
check('scene 子节点=2（立方体+坐标轴）', scene.children.length === 2);
cube.rotation.y += 0.008;
cube.rotation.x += 0.004;
camera.aspect = 4 / 3;
camera.updateProjectionMatrix();
scene.updateMatrixWorld(); // 无异常即通过
check('自转/纵横比更新/矩阵更新无异常', true);

// 释放路径（index.html closeLayer 的非 DOM 部分）
geo.dispose();
mat.dispose();
check('geometry/material dispose 无异常', true);

console.log(fail.length === 0 ? '== 全部通过 ==' : '== ' + fail.length + ' 项失败 ==');
process.exit(fail.length === 0 ? 0 : 1);
