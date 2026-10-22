
  // ==== D-004a：Three.js 空场景共存层（渲染升级 D-004 第 1 步第③件）====
  //
  // 混搭架构锚（官方混搭指南 pixijs.com/8.x/guides/third-party/mixing-three-and-pixi）：
  // - 本层 Three 持有页面上唯一的 WebGL context；未来 PixiJS 层不再自建 context，
  //   以 pixiRenderer.init({ context: threeRenderer.getContext(),
  //   clearBeforeRender: false }) 与 Three 共享同一 context 混画（Pixi 画 2D/UI，
  //   Three 画 3D 世界，一帧双引擎）。
  // - 当前形态：Canvas 2D 战场与 Three 是「两层独立 canvas 叠加」（本层 z-index:1
  //   在上、pointer-events:none 不吞点击）；PixiJS 引入时（后续刀）替换 Canvas 2D
  //   层——届时收敛为 Pixi(2D/UI) + Three(3D) 共享 context 的定案形态。
  // - 生命周期：开 = 建 renderer + 场景 + rAF 循环；关 = cancelAnimationFrame +
  //   几何/材质/renderer 全量 dispose + forceContextLoss + canvas 移除——零后台
  //   循环、零 context 泄漏，反复开关可重建。
  import * as THREE from './vendor/three.module.min.js';

  var stageEl = document.getElementById('stage');
  var btnEl = document.getElementById('threeBtn');
  var statusEl = document.getElementById('threeStatus');

  var layer = null; // 开启态句柄 {canvas, renderer, scene, camera, cube, geo, mat, raf}；null=关

  function stageSize() {
    return { w: Math.max(1, stageEl.clientWidth), h: Math.max(1, stageEl.clientHeight) };
  }

  // 建层：透明 WebGL 画布 + 空场景最小可信物（自转彩色线框立方体 + RGB 坐标轴）
  function openLayer() {
    if (layer) return;
    var canvas = document.createElement('canvas');
    canvas.id = 'threeCanvas';
    stageEl.appendChild(canvas);
    var renderer;
    try {
      renderer = new THREE.WebGLRenderer({ canvas: canvas, alpha: true, antialias: true });
    } catch (e) {
      // WebGL 不可用：移除画布如实降级——3D 层不开，2D 战场照常
      canvas.remove();
      console.warn('[cw3d] WebGL 初始化失败，3D 层不可用');
      return;
    }
    renderer.setPixelRatio(window.devicePixelRatio || 1);
    renderer.setClearColor(0x000000, 0); // 背景全透明，2D 战场透出来
    var sz = stageSize();
    renderer.setSize(sz.w, sz.h, false); // updateStyle=false：显示尺寸全由 CSS 100% 控制

    var scene = new THREE.Scene();
    var camera = new THREE.PerspectiveCamera(50, sz.w / sz.h, 0.1, 100);
    camera.position.set(2.4, 1.8, 2.6);
    camera.lookAt(0, 0, 0);

    // 彩色线框立方体：顶点色按坐标渐变（r=x / g=y / b=z），转起来立体感一眼可辨
    var geo = new THREE.BoxGeometry(1, 1, 1);
    var pos = geo.attributes.position;
    var colors = new Float32Array(pos.count * 3);
    for (var i = 0; i < pos.count; i++) {
      colors[i * 3]     = 0.3 + (pos.getX(i) + 0.5) * 0.7; // x -> 红
      colors[i * 3 + 1] = 0.3 + (pos.getY(i) + 0.5) * 0.7; // y -> 绿
      colors[i * 3 + 2] = 0.3 + (pos.getZ(i) + 0.5) * 0.7; // z -> 蓝
    }
    geo.setAttribute('color', new THREE.BufferAttribute(colors, 3));
    var mat = new THREE.MeshBasicMaterial({ wireframe: true, vertexColors: true });
    var cube = new THREE.Mesh(geo, mat);
    scene.add(cube);
    scene.add(new THREE.AxesHelper(1.5)); // RGB 坐标轴固定不转，立方体绕轴自转（参照系清晰）

    layer = { canvas: canvas, renderer: renderer, scene: scene, camera: camera,
              cube: cube, geo: geo, mat: mat, raf: 0 };
    (function frame() {
      if (!layer) return; // 关层后可能仍有一帧在途：null 即停，绝不复活循环
      layer.cube.rotation.y += 0.008; // 缓慢自转
      layer.cube.rotation.x += 0.004;
      layer.renderer.render(layer.scene, layer.camera);
      layer.raf = requestAnimationFrame(frame);
    })();

    btnEl.textContent = '3D 层：开';
    statusEl.textContent = '3D: 开(r' + THREE.REVISION + ')';
    console.info('[cw3d] 3D 层开启（three r' + THREE.REVISION + '）');
  }

  // 关层：动画停 + 资源全量释放 + 画布移除（底栏状态归零；重开=全新重建）
  function closeLayer() {
    if (!layer) return;
    cancelAnimationFrame(layer.raf);
    layer.geo.dispose();
    layer.mat.dispose();
    layer.renderer.dispose();
    layer.renderer.forceContextLoss();
    layer.canvas.remove();
    layer = null;
    btnEl.textContent = '3D 层：关';
    statusEl.textContent = '';
    console.info('[cw3d] 3D 层关闭（renderer 已释放）');
  }

  function toggle() {
    if (layer) closeLayer();
    else openLayer();
  }

  btnEl.addEventListener('click', toggle);
  window.addEventListener('keydown', function (ev) {
    if (ev.key === 't' || ev.key === 'T') toggle(); // 双入口之二：快捷键（按钮见上）
  });

  // 窗口尺寸变化：3D 画布开着就同步（CSS 100% 只拉伸显示，重设绘制缓冲+相机
  // 纵横比才不糊不变形；与经典脚本 resize() 各自监听同一事件，互不干扰）
  window.addEventListener('resize', function () {
    if (!layer) return;
    var sz = stageSize();
    layer.renderer.setSize(sz.w, sz.h, false);
    layer.camera.aspect = sz.w / sz.h;
    layer.camera.updateProjectionMatrix();
  });
