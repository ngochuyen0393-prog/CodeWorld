
  // ==== D-004b：卡通渲染测试角色场景（渲染升级 D-004 第 2 步）====
  //
  // 混搭架构锚（D-004a 定案）：Canvas 2D 战场与 Three 是「两层独立 canvas 叠加」；
  // 本层 Three 持有页面上唯一的 WebGL context。生命周期：开 = 建 renderer +
  // 加载模型贴图 + rAF 循环；关 = cancelAnimationFrame + 材质/几何/深度RT/
  // KTX2 worker/renderer 全量 dispose + forceContextLoss + canvas 移除——零后台
  // 循环、零 context 泄漏，反复开关可重建。
  //
  // 三视角轮播：正脸/左45°/右45° 各停 3s + 0.6s 缓动过渡，验证 FaceSDF 阴影
  // 方向敏感性（左右脸阴影应对称）；用户在 3D 画布 pointerdown 即停轮播转手动
  // OrbitControls。still 模式（#d3d-still）不轮播且冻结 uTime（视觉回归截图
  // 稳定帧）；full 模式（#d3d-*-full）3D 画布临时全窗口铺满（相机 aspect 与
  // KallkaGo 全屏 demo 一致，对比截图用）。
  import * as THREE from 'three';
  import { GLTFLoader } from 'three/addons/loaders/GLTFLoader.js';
  import { KTX2Loader } from 'three/addons/loaders/KTX2Loader.js';
  import { OrbitControls } from 'three/addons/controls/OrbitControls.js';
  import { GenshinToonMaterial } from './genshin-toon/GenshinToonMaterial.js';

  var stageEl = document.getElementById('stage');
  var btnEl = document.getElementById('threeBtn');
  var statusEl = document.getElementById('threeStatus');
  var matBtnEl = document.getElementById('matBtn');
  var matPanelEl = document.getElementById('matPanel');
  var matPanelCloseEl = document.getElementById('matPanelClose');

  // 截图/验证入口：hash 含 d3d 自动开层；still=停轮播冻帧；full=3D 画布全窗口
  var HASH = location.hash || '';
  var STILL_MODE = HASH.indexOf('still') >= 0;
  var FULL_MODE = HASH.indexOf('full') >= 0;

  // 3D 层加载链路观测：#d3d* 模式下关键节点上报服务端日志（无头截图看不到
  // F12 console，加载卡死时靠这个定位断在哪一层）
  function cw3dLog(msg) {
    if (HASH.indexOf('d3d') < 0) return;
    console.info('[cw3d] ' + msg);
    try { fetch('/__log?m=' + encodeURIComponent(String(msg).slice(0, 4000))).catch(function () {}); } catch (e) { /* 尽力上报 */ }
  }
  // console.error 透传上报（three 的 shader 编译错/WebGL 报错都走这里）
  (function () {
    var origErr = console.error;
    console.error = function () {
      var parts = Array.prototype.slice.call(arguments).map(function (a) {
        try { return typeof a === 'string' ? a.slice(0, 4000) : (a && a.message ? String(a.message).slice(0, 4000) : JSON.stringify(a).slice(0, 500)); } catch (e) { return String(a); }
      });
      cw3dLog('console.error: ' + parts.join(' | '));
      origErr.apply(console, arguments);
    };
  })();

  var layer = null; // 开启态句柄；null=关。字段：canvas/renderer/scene/camera/controls/
                    // group/rigged/depth/ktx2/raf/lastT + 轮播状态 auto/viewIdx/...
  var matPanelBound = false; // 材质面板滑条只绑一次（反复开关层不叠加监听器）

  // 三视角底账：rotation.y 0 / +45° / -45°（正脸/左45/右45），停 3s + 0.6s 缓动
  var VIEW_RY = [0, Math.PI / 4, -Math.PI / 4];
  var HOLD_MS = 3000, ROT_MS = 600;

  function stageSize() {
    // full 模式（对比截图）：3D 画布全窗口铺满，尺寸对齐窗口（相机 aspect 与
    // KallkaGo 全屏 demo 一致；stage 内嵌会吃掉 caption/bar 高度导致 aspect 不齐）
    if (FULL_MODE) {
      return { w: Math.max(1, window.innerWidth), h: Math.max(1, window.innerHeight) };
    }
    return { w: Math.max(1, stageEl.clientWidth), h: Math.max(1, stageEl.clientHeight) };
  }

  // 9 张贴图并行加载（路径照 RES.ts 底账；colorSpace/mipmap 由材质内部自动设置）
  function loadTextures() {
    var texLoader = new THREE.TextureLoader();
    return Promise.all([
      texLoader.loadAsync('./textures/face/faceLightmap.png'),
      texLoader.loadAsync('./textures/hair/light.png'),
      texLoader.loadAsync('./textures/body/light.png'),
      texLoader.loadAsync('./textures/hair/ramp.png'),
      texLoader.loadAsync('./textures/body/ramp.png'),
      texLoader.loadAsync('./textures/body/emissive.png'),
      texLoader.loadAsync('./textures/matcap/metalMap.png'),
      texLoader.loadAsync('./textures/hair/normal.png'),
      texLoader.loadAsync('./textures/body/normal.png'),
    ]).then(function (t) {
      return {
        faceLightMap: t[0], hairLightMap: t[1], bodyLightMap: t[2],
        hairRampMap: t[3], bodyRampMap: t[4], emissiveMap: t[5],
        metalMap: t[6], hairNormalMap: t[7], bodyNormalMap: t[8],
      };
    });
  }

  // 建层：透明 WebGL 画布 + 卡通角色场景
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
    renderer.setClearColor(0x000000, 0); // 背景全透明，2D 战场透出来（KallkaGo transparent 底账）
    var sz = stageSize();
    renderer.setSize(sz.w, sz.h, false); // updateStyle=false：显示尺寸全由 CSS 100% 控制

    var scene = new THREE.Scene();
    // 相机底账：fov50 / near0.1 / far200 / pos(0,0,2)（照参考 demo）
    var camera = new THREE.PerspectiveCamera(50, sz.w / sz.h, 0.1, 200);
    camera.position.set(0, 0, 2);
    camera.lookAt(0, 0, 0);

    layer = {
      canvas: canvas, renderer: renderer, scene: scene, camera: camera,
      controls: null, group: null, rigged: null, depth: null, ktx2: null,
      raf: 0, lastT: performance.now(),
      auto: !STILL_MODE, viewIdx: 0, phase: 'hold', phaseT: 0, fromRy: 0, toRy: 0,
    };
    statusEl.textContent = '3D: 加载中…';
    console.info('[cw3d] 3D 层开启（three r' + THREE.REVISION + '，加载绫华中）');

    buildScene().then(function (built) {
      if (!layer) { disposeBuilt(built); return; } // 加载中已被关层：立即释放半成品
      Object.assign(layer, built);
      afterLoad();
    }).catch(function (err) {
      cw3dLog('加载失败: ' + (err && (err.stack || err.message || err)));
      console.error('[cw3d] 角色场景加载失败：', err);
      if (layer) closeLayer();
      statusEl.textContent = '3D: 加载失败(见console)';
    });
  }

  // 场景装配：贴图/模型并行加载 -> 材质分派 -> 深度 provider
  async function buildScene() {
    var renderer = layer.renderer, scene = layer.scene, camera = layer.camera;

    var textures = await loadTextures();
    cw3dLog('贴图加载完成(9张)');
    if (!layer) throw new Error('closed');

    var ktx2 = new KTX2Loader().setTranscoderPath('./vendor/basis/').detectSupport(renderer);
    var gltfLoader = new GLTFLoader().setKTX2Loader(ktx2);
    // GLB 实测无 KHR_draco_mesh_compression 扩展（D-004b 二进制检测），
    // 不接 DRACOLoader——少一份解码器死资产
    var gltf = await gltfLoader.loadAsync('./models/ayakaktx2opt.glb');
    cw3dLog('glb 加载+KTX2解码完成: meshes=' + (gltf.scene ? gltf.scene.children.length : '?'));
    if (!layer) throw new Error('closed');

    // 环境光底账 #fff3e3/1.02（Sketch.tsx）：场景对象存档语义；shader 侧的
    // ambient 间接光贡献由 setAmbientLight 同值提供（预除 PI，见 common.js）
    scene.add(new THREE.AmbientLight(0xfff3e3, 1.02));
    GenshinToonMaterial.setAmbientLight('#fff3e3', 1.02);

    var group = new THREE.Group();
    group.position.set(0, -0.7, 0); // 模型偏移（Ayaka 底账 y=-0.7）
    group.add(gltf.scene);
    scene.add(group);

    var rigged = GenshinToonMaterial.applyToSkeleton(gltf.scene, { textures: textures });
    var depth = GenshinToonMaterial.attachDepthProvider(renderer, scene, camera, [rigged.outlineRoot]);

    return { group: group, rigged: rigged, depth: depth, ktx2: ktx2 };
  }

  // 装配完成：控制器 + 三视角 + 面板 + rAF 主循环
  function afterLoad() {
    var canvas = layer.canvas, camera = layer.camera, renderer = layer.renderer;
    var rigged = layer.rigged;

    // OrbitControls 底账：minDistance 0.5 / maxDistance 10（Sketch.tsx 同款），
    // 绑 3D 画布（pointer-events:auto 才能收拖拽；3D 开启时占屏属可接受，关闭即还原）
    var controls = new OrbitControls(camera, canvas);
    controls.minDistance = 0.5;
    controls.maxDistance = 10;
    controls.enabled = false; // 轮播期间禁用；用户按下即接管
    layer.controls = controls;
    canvas.classList.add('interactive'); // pointer-events:none -> auto（CSS 类切换）
    canvas.addEventListener('pointerdown', function () {
      if (!layer) return;
      layer.auto = false;          // 用户接管：停轮播转手动
      controls.enabled = true;
    });

    syncResolution();

    // 材质面板：滑条直改共享 uniforms（任一实例做代理即全量生效）。
    // 面板滑条是固定 DOM，绑定一次防反复开关层叠加监听器；apply 闭包每次取
    // 当前层的材质实例（rigged 换新后旧引用不残留）
    matBtnEl.hidden = false;
    if (!matPanelBound) {
      matPanelBound = true;
      bindSlider('s-rimW', 'v-rimW', function (v) { if (layer) layer.rigged.materials[0].rimWidth = v; });
      bindSlider('s-rimI', 'v-rimI', function (v) { if (layer) layer.rigged.materials[0].rimIntensity = v; });
      bindSlider('s-outW', 'v-outW', function (v) { if (layer) layer.rigged.materials[0].outlineWidth = v; });
      bindSlider('s-day', 'v-day', function (v) { if (layer) layer.rigged.materials[0].isDay = v; });
      bindSlider('s-metal', 'v-metal', function (v) { if (layer) layer.rigged.materials[0].metallic = v; });
      bindSlider('s-sdf', 'v-sdf', function (v) { if (layer) layer.rigged.materials[0].intensity = v; });
      // D-004c：天使圈/rim 泛化四滑条（rimHue 经 setter 换算 hue→RGB 进 uRimColor）
      bindSlider('s-anisoI', 'v-anisoI', function (v) { if (layer) layer.rigged.materials[0].anisoIntensity = v; });
      bindSlider('s-anisoShift', 'v-anisoShift', function (v) { if (layer) layer.rigged.materials[0].anisoShift = v; });
      bindSlider('s-rimHue', 'v-rimHue', function (v) { if (layer) layer.rigged.materials[0].rimHue = v; });
      bindSlider('s-rimShade', 'v-rimShade', function (v) { if (layer) layer.rigged.materials[0].rimShadeMix = v; });
    }

    if (STILL_MODE) {
      layer.rigged.shared.uTime.value = 1.0; // 冻帧：自发光呼吸固定（视觉回归稳定帧）
      // still 下可指定初始视角（三视角截图入口）：#d3d-still-l45 / -r45
      if (HASH.indexOf('l45') >= 0) { layer.viewIdx = 1; layer.group.rotation.y = VIEW_RY[1]; }
      else if (HASH.indexOf('r45') >= 0) { layer.viewIdx = 2; layer.group.rotation.y = VIEW_RY[2]; }
      console.info('[cw3d] still 模式：停轮播 + uTime 冻结');
    }

    // D-004c 开/关对照开关：hash 追加 &a=0/1（天使圈关/开）、&r=0/1（rim 泛化关/开）。
    // 缺省不动默认底账；a=1 显式回 0.6（与默认等价），r=1 开演示档（rimHue 0.6 青蓝 + 背光压暗 0.6）。
    // 无头截图与手动 #d3d 亲验共用同一入口。
    function hashFlag(key) {
      var m = HASH.match(new RegExp('[&#]' + key + '=([01])'));
      return m ? m[1] : null;
    }
    var aFlag = hashFlag('a');
    if (aFlag === '0') { layer.rigged.materials[0].anisoIntensity = 0; }
    else if (aFlag === '1') { layer.rigged.materials[0].anisoIntensity = 0.6; }
    var rFlag = hashFlag('r');
    if (rFlag === '1') {
      layer.rigged.materials[0].rimHue = 0.6;
      layer.rigged.materials[0].rimShadeMix = 0.6;
    } else if (rFlag === '0') {
      layer.rigged.materials[0].rimHue = 0;
      layer.rigged.materials[0].rimShadeMix = 0;
    }
    // 无头日志验证锚：天使圈/rim 当前生效值（铁律 4；cw3dLog 同时上报 /__log）
    cw3dLog('天使圈 intensity=' + layer.rigged.materials[0].anisoIntensity +
      ' shift=' + layer.rigged.materials[0].anisoShift +
      '；rim 颜色=#' + layer.rigged.materials[0].uniforms.uRimColor.value.getHexString() +
      ' 背光压暗=' + layer.rigged.materials[0].rimShadeMix);

    btnEl.textContent = '3D 层：开';
    statusEl.textContent = '3D: 开(r' + THREE.REVISION + '·绫华)';
    cw3dLog('渲染就绪（材质 ' + rigged.materials.length + ' + 描边 ' +
      rigged.outlineMaterials.length + '；_uv7=' +
      (rigged.outlineMaterials.some(function (m) { return m.defines.CW_HAS_UV7; }) ? '有' : '无(退化)') + '）');
    // 诊断：每个 mesh 的 geometry attribute 清单 + 材质 defines（shader 编译错定位用）
    layer.group.traverse(function (o) {
      if (o.isMesh) {
        cw3dLog('mesh=' + (o.material.name || '?') + ' attrs=[' +
          Object.keys(o.geometry.attributes).join(',') + '] defines=' +
          JSON.stringify(o.material.defines || {}));
      }
    });

    layer.lastT = performance.now();
    var firstFrame = true;
    (function frame(now) {
      if (!layer) return; // 关层后可能仍有一帧在途：null 即停，绝不复活循环
      var dtMs = Math.min(now - layer.lastT, 100);
      layer.lastT = now;
      var dt = dtMs / 1000;

      layer.depth.prePass();          // 帧序① depth pre-pass（rim 深度采样源）
      if (layer.auto) tickViews(dtMs); // 帧序② 前置：三视角轮播推进（改 rotation）
      if (!STILL_MODE) layer.rigged.materials[0].update(dt); // uTime 共享推进
      layer.renderer.render(layer.scene, layer.camera); // 帧序②③ outline+body 主 pass
      if (firstFrame) { // 首帧渲染完成信号（still 截图观察点）
        firstFrame = false;
        if (firstFrameResolve) { firstFrameResolve(); firstFrameResolve = null; }
        cw3dLog('首帧渲染完成');
      }
      layer.raf = requestAnimationFrame(frame);
    })(performance.now());
  }

  // 三视角轮播状态机：HOLD(3s) -> ROT(0.6s easeInOutQuad) -> 下一视角
  function tickViews(dtMs) {
    layer.phaseT += dtMs;
    if (layer.phase === 'hold') {
      if (layer.phaseT >= HOLD_MS) {
        layer.fromRy = layer.group.rotation.y;
        layer.viewIdx = (layer.viewIdx + 1) % VIEW_RY.length;
        layer.toRy = VIEW_RY[layer.viewIdx];
        layer.phase = 'rot';
        layer.phaseT = 0;
      }
    } else {
      var k = Math.min(1, layer.phaseT / ROT_MS);
      var e = k < 0.5 ? 2 * k * k : 1 - Math.pow(-2 * k + 2, 2) / 2; // easeInOutQuad
      layer.group.rotation.y = layer.fromRy + (layer.toRy - layer.fromRy) * e;
      if (k >= 1) {
        layer.phase = 'hold';
        layer.phaseT = 0;
      }
    }
  }

  // 材质面板滑条绑定：input 事件实时生效 + 数值回显
  function bindSlider(sid, vid, apply) {
    var el = document.getElementById(sid);
    var out = document.getElementById(vid);
    el.addEventListener('input', function () {
      var v = parseFloat(el.value);
      out.textContent = String(v);
      apply(v);
    });
  }

  // 分辨率同步：uResolution = 绘制缓冲像素（shader 的 SDF 邻域/描边 aspect 用）
  function syncResolution() {
    var s = layer.renderer.getDrawingBufferSize(new THREE.Vector2());
    var shared = layer.rigged.shared;
    shared.uResolution.value.set(s.x, s.y);
  }

  // 半成品释放（加载中途被关层）：贴图/材质还没进共享表全量，能释放多少释放多少
  function disposeBuilt(built) {
    try { if (built.depth) built.depth.dispose(); } catch (e) { /* 半成品尽力释放 */ }
    try { if (built.ktx2) built.ktx2.dispose(); } catch (e) { /* 同上 */ }
    console.info('[cw3d] 加载中关闭：半成品已释放');
  }

  // 关层：动画停 + 资源全量释放 + 画布移除（底栏/面板状态归零；重开=全新重建）
  function closeLayer() {
    if (!layer) return;
    cancelAnimationFrame(layer.raf);
    var layerRef = layer;
    layer = null; // 先置空：在途 async 回调/rAF 一律短路
    if (layerRef.depth) layerRef.depth.dispose();
    if (layerRef.rigged) {
      layerRef.rigged.materials.forEach(function (m) { m.dispose(); });
      layerRef.rigged.outlineMaterials.forEach(function (m) { m.dispose(); });
      // 几何：描边克隆与主体共享 geometry，traverse 主树 dispose 一次即可
      layerRef.group.traverse(function (o) { if (o.isMesh && o.geometry) o.geometry.dispose(); });
    }
    if (layerRef.ktx2) layerRef.ktx2.dispose();
    if (layerRef.controls) layerRef.controls.dispose();
    layerRef.renderer.dispose();
    layerRef.renderer.forceContextLoss();
    layerRef.canvas.remove();
    GenshinToonMaterial.resetShared(); // 共享 uniform 表随层销毁（防跨层串值）
    matBtnEl.hidden = true;
    matPanelEl.hidden = true;
    btnEl.textContent = '3D 层：关';
    statusEl.textContent = '';
    console.info('[cw3d] 3D 层关闭（renderer/材质/深度RT 已释放）');
  }

  function toggle() {
    if (layer) closeLayer();
    else openLayer();
  }

  matBtnEl.addEventListener('click', function () {
    matPanelEl.hidden = !matPanelEl.hidden;
  });
  matPanelCloseEl.addEventListener('click', function () {
    matPanelEl.hidden = true;
  });

  btnEl.addEventListener('click', toggle);
  window.addEventListener('keydown', function (ev) {
    if (ev.key === 't' || ev.key === 'T') toggle(); // 双入口之二：快捷键（按钮见上）
  });

  // URL hash 入口：#d3d 自动开 3D 层（无头验证 + 重开），#d3d-still 停轮播冻帧
  var firstFrameResolve = null;
  if (HASH.indexOf('d3d') >= 0) {
    openLayer();
    // 无头截图时序锚：#d3d-still 下注入一个「非 async 动态 script」（服务端
    // /__hold/N 保持 N 秒才响应）——动态非 async script 会推迟 load 事件但
    // 不占主线程（此刻模型/KTX2 已在异步加载），无头截图在 load 后即拍，
    // 由此锚定在「首帧渲染完成后、画面静止」的稳定窗口。head 里放 hold
    // script 的方案是错的：parser 阻塞会连带推迟 defer module 的执行。
    if (STILL_MODE) {
      var hold = document.createElement('script');
      hold.src = '/__hold/20000';
      hold.async = false;
      document.head.appendChild(hold);
    }
  }

  // 窗口尺寸变化：3D 画布开着就同步（CSS 100% 只拉伸显示，重设绘制缓冲+相机
  // 纵横比才不糊不变形；与经典脚本 resize() 各自监听同一事件，互不干扰）
  window.addEventListener('resize', function () {
    if (!layer) return;
    var sz = stageSize();
    layer.renderer.setSize(sz.w, sz.h, false);
    layer.camera.aspect = sz.w / sz.h;
    layer.camera.updateProjectionMatrix();
    if (layer.rigged) syncResolution();
  });

  // full 模式（对比截图用）：3D 画布全窗口铺满 + 隐藏 2D 层 UI（caption/bar/
  // 断线横幅入镜会污染逐像素对比；正常使用不受影响，仅截图 hash 生效）
  if (FULL_MODE) {
    var style = document.createElement('style');
    style.textContent =
      '#threeCanvas { position: fixed !important; inset: 0 !important; width: 100vw !important; height: 100vh !important; }' +
      '#cv { display: none !important; }' + // 2D 战场层（含等待文案）一并隐藏——污染对比
      '#caption, #bar, #dcBanner, #logoutBanner, #tip, #matPanel { display: none !important; }';
    document.head.appendChild(style);
    window.dispatchEvent(new Event('resize')); // 已开层时补一次尺寸同步
  }
