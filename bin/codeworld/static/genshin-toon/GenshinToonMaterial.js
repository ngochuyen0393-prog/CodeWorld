// Based on KallkaGo/ToonShading (MIT) — Copyright (c) 2024 KallkaGo
// GenshinToonMaterial —— 原神式卡通渲染独立材质（three r160，零构建链 ES module）。
//
// 【架构决策 · MToon 同款】不依赖 three-custom-shader-material（CSM）、不依赖
// React、不依赖构建链：自带完整 vertex/fragment main()，主光方向由 uLightPosition
// uniform 提供，不接 three 灯照循环（lights:false）。ambient 间接光贡献与
// ACES/sRGB 输出链在 shader 内手工复刻（shaders/common.js），与 KallkaGo
// 「CSM 挂 MeshStandardMaterial」的内置收尾等价。
//
// 【CSM mipmap 工具不搬】上游 PackedMipMapGenerator 仅服务于 Bloom/反射管线
// 的 packed mip 采样；本卡管线 ramp/ILM 全程 generateMipmaps=false（卡通色阶
// 要锐利边界，mip 平均反而糊），故该工具不移植。
//
// 【共享 uniforms】同一骨架的 face/body/描边材质共享一份全局 uniform 表
// （uTime/uLightPosition/uIsDay/...），面板滑条改任一材质的代理属性即全量生效。
// 换场景/关层时调 GenshinToonMaterial.resetShared() 防跨层串值。

import * as THREE from 'three';
import { TOON_VERTEX } from './shaders/vertex.js';
import { TOON_BODY_FRAG } from './shaders/body-frag.js';
import { TOON_FACE_FRAG } from './shaders/face-frag.js';
import { OUTLINE_VERTEX } from './shaders/outline-vertex.js';
import { OUTLINE_FRAG } from './shaders/outline-frag.js';

/* ---------- 贴图 colorSpace/mipmap 底账（照抄 Ayaka.tsx 61-84，材质内部自动设，调用方零贴图知识） ---------- */

function prepILM(t) { // faceLightMap/hairLightMap/bodyLightMap：线性 + 无 mip + 不翻转
  if (!t) return t;
  t.colorSpace = THREE.LinearSRGBColorSpace;
  t.generateMipmaps = false;
  t.flipY = false;
  return t;
}
function prepNormal(t) { // normal 系：不翻转 + 无 mip（线性，Ayaka 未设 colorSpace 即线性）
  if (!t) return t;
  t.colorSpace = THREE.LinearSRGBColorSpace;
  t.generateMipmaps = false;
  t.flipY = false;
  return t;
}
function prepRamp(t) { // ramp 系：sRGB + 无 mip（flipY 保持调用方状态，Ayaka 同款）
  if (!t) return t;
  t.colorSpace = THREE.SRGBColorSpace;
  t.generateMipmaps = false;
  return t;
}
function prepEmissive(t) { // 自发光：sRGB + 无 mip + 不翻转
  if (!t) return t;
  t.colorSpace = THREE.SRGBColorSpace;
  t.generateMipmaps = false;
  t.flipY = false;
  return t;
}
function prepBase(t) { // glb 内嵌 baseMap：sRGB（GLTFLoader 已设，这里兜底确保）
  if (!t) return t;
  t.colorSpace = THREE.SRGBColorSpace;
  return t;
}

/* 未接深度 provider 时的兜底深度纹理：1x1 全 1（=远平面）→ 深度差恒 0 → rim 归零退化不崩 */
let _blankDepth = null;
function getBlankDepth() {
  if (!_blankDepth) {
    _blankDepth = new THREE.DataTexture(new Uint8Array([255, 255, 255, 255]), 1, 1);
    _blankDepth.needsUpdate = true;
  }
  return _blankDepth;
}

export class GenshinToonMaterial extends THREE.ShaderMaterial {
  /* ---- 共享 uniform 表（每场景一份） ---- */
  static #shared = null;

  static ensureShared(options = {}) {
    if (GenshinToonMaterial.#shared) return GenshinToonMaterial.#shared;
    const shared = {
      uLightPosition: new THREE.Uniform(new THREE.Vector3(0, 5, 5)), // 参考底账：主光默认 (0,5,5)
      uIsDay: new THREE.Uniform(0.5),          // 白天
      uShadowColor: new THREE.Uniform(new THREE.Color(0xffffff)),
      uNoMetallic: new THREE.Uniform(1),
      uMetallic: new THREE.Uniform(0.5),
      uRimLightWidth: new THREE.Uniform(1),
      uRimLightIntensity: new THREE.Uniform(1),
      uTime: new THREE.Uniform(0),
      uNear: new THREE.Uniform(0.1),
      uFar: new THREE.Uniform(200),
      uResolution: new THREE.Uniform(new THREE.Vector2(1, 1)), // 绘制缓冲像素（页面 resize 时更新）
      uIntensity: new THREE.Uniform(2),        // face SDF 强度（Ayaka.tsx:119 默认 2）
      uEmissiveIntensity: new THREE.Uniform(0.2), // 原 shader 硬编码 .2 参数化
      uAmbientColor: new THREE.Uniform(new THREE.Color(0, 0, 0)), // JS 侧预除 PI
      uToneMapped: new THREE.Uniform(0),       // ACES 默认关（KallkaGo 是 NoToneMapping，见下）
      /* GT tone map 复刻（KallkaGo 后处理 GTToneMap，参数=Sketch.tsx leva 底账默认）：
         KallkaGo 的 renderer 是 NoToneMapping，画面 tone 曲线来自这个后处理 */
      uGTMaxLum: new THREE.Uniform(2),
      uGTContrast: new THREE.Uniform(1),
      uGTLinearStart: new THREE.Uniform(0.1),
      uGTLinearLen: new THREE.Uniform(0.12),
      uGTBlackC: new THREE.Uniform(1.2),
      uGTBlackB: new THREE.Uniform(0),
      uGTEnabled: new THREE.Uniform(1),
      uOutLineWidth: new THREE.Uniform(0.4),   // 描边宽（Ayaka uniforms 初始 0.4）
    };
    // 构造参数覆写默认底账
    if (options.isDay !== undefined) shared.uIsDay.value = options.isDay;
    if (options.rimWidth !== undefined) shared.uRimLightWidth.value = options.rimWidth;
    if (options.rimIntensity !== undefined) shared.uRimLightIntensity.value = options.rimIntensity;
    if (options.outlineWidth !== undefined) shared.uOutLineWidth.value = options.outlineWidth;
    if (options.metallic !== undefined) shared.uMetallic.value = options.metallic;
    if (options.noMetallic !== undefined) shared.uNoMetallic.value = options.noMetallic;
    if (options.emissiveIntensity !== undefined) shared.uEmissiveIntensity.value = options.emissiveIntensity;
    if (options.intensity !== undefined) shared.uIntensity.value = options.intensity;
    if (options.lightPosition) shared.uLightPosition.value.copy(options.lightPosition);
    if (options.shadowColor) shared.uShadowColor.value.set(options.shadowColor);
    if (options.toneMapped !== undefined) shared.uToneMapped.value = options.toneMapped;
    GenshinToonMaterial.#shared = shared;
    return shared;
  }

  static resetShared() {
    GenshinToonMaterial.#shared = null;
  }

  /* 场景环境光（对应 KallkaGo 场景 AmbientLight 对内置材质的间接光贡献）：
     color 走 ColorManagement 自动转 linear，强度预除 PI（BRDF_Lambert） */
  static setAmbientLight(color, intensity) {
    const shared = GenshinToonMaterial.ensureShared();
    shared.uAmbientColor.value.copy(new THREE.Color(color)).multiplyScalar(intensity / Math.PI);
  }

  static get sharedUniforms() {
    return GenshinToonMaterial.#shared;
  }

  constructor(options = {}) {
    const shared = GenshinToonMaterial.ensureShared(options);
    const isFace = options.isFace === true;
    const c = options.baseColor || new THREE.Color(1, 1, 1); // glb 材质 baseColorFactor（linear）
    const baseColor = new THREE.Vector4(c.r, c.g, c.b, 1);
    const uniforms = {
      /* 贴图（per-实例） */
      uBaseMap: { value: prepBase(options.baseMap || null) },
      uBaseColor: { value: baseColor },
      uLightMap: { value: prepILM(options.lightMap || null) },
      uRampMap: { value: prepRamp(options.rampMap || null) },
      uMetalMap: { value: options.metalMap || null }, // Ayaka 未对 metalMap 做任何设置，照抄不动
      uNormalMap: { value: prepNormal(options.normalMap || null) },
      uEmissiveMap: { value: prepEmissive(options.emissiveMap || null) },
      uFaceLightMap: { value: prepILM(options.faceSDFMap || null) },
      uDepthTexture: { value: options.depthTexture || getBlankDepth() },
      /* 朝向（face SDF 用） */
      uForwardVec: { value: (options.forwardVec || new THREE.Vector3(0, 0, 1)).clone() },
      uRightVec: { value: (options.rightVec || new THREE.Vector3(1, 0, 0)).clone() },
      /* 共享表（全量挂上，shader 各取所需；面板改一处全生效） */
      uLightPosition: shared.uLightPosition,
      uIsDay: shared.uIsDay,
      uShadowColor: shared.uShadowColor,
      uNoMetallic: shared.uNoMetallic,
      uMetallic: shared.uMetallic,
      uRimLightWidth: shared.uRimLightWidth,
      uRimLightIntensity: shared.uRimLightIntensity,
      uTime: shared.uTime,
      uNear: shared.uNear,
      uFar: shared.uFar,
      uResolution: shared.uResolution,
      uIntensity: shared.uIntensity,
      uEmissiveIntensity: shared.uEmissiveIntensity,
      uAmbientColor: shared.uAmbientColor,
      uToneMapped: shared.uToneMapped,
      /* GT tone map 复刻参数（common.js toonFinalColor 用） */
      uGTMaxLum: shared.uGTMaxLum,
      uGTContrast: shared.uGTContrast,
      uGTLinearStart: shared.uGTLinearStart,
      uGTLinearLen: shared.uGTLinearLen,
      uGTBlackC: shared.uGTBlackC,
      uGTBlackB: shared.uGTBlackB,
      uGTEnabled: shared.uGTEnabled,
    };
    super({
      name: options.name || (isFace ? 'GenshinToonFace' : 'GenshinToonBody'),
      defines: options.defines || {}, // geometry 缺 attribute 时的退化开关（如 CW_NO_TANGENT）
      uniforms,
      vertexShader: TOON_VERTEX,
      fragmentShader: isFace ? TOON_FACE_FRAG : TOON_BODY_FRAG,
      lights: false, // 不接 three 灯照循环：主光方向由 uLightPosition 提供（MToon 同款架构）
      transparent: options.transparent ?? false,
      side: options.side ?? THREE.FrontSide,
      depthWrite: options.depthWrite ?? true,
      depthTest: options.depthTest ?? true,
    });
    GenshinToonMaterial._register(this);
    this.addEventListener('dispose', () => GenshinToonMaterial._unregister(this));
  }

  /* 所有实例共享 uTime：页面每帧对任一实例调一次 update(delta) 即可 */
  update(delta) {
    this.uniforms.uTime.value += delta;
  }

  /* ---- getter/setter 代理（MToon 笔记启示：直达 uniforms 值） ---- */
  get lightPosition() { return this.uniforms.uLightPosition.value; }
  set lightPosition(v) { this.uniforms.uLightPosition.value.copy(v); }
  get rimWidth() { return this.uniforms.uRimLightWidth.value; }
  set rimWidth(v) { this.uniforms.uRimLightWidth.value = v; }
  get rimIntensity() { return this.uniforms.uRimLightIntensity.value; }
  set rimIntensity(v) { this.uniforms.uRimLightIntensity.value = v; }
  get isDay() { return this.uniforms.uIsDay.value; }
  set isDay(v) { this.uniforms.uIsDay.value = v; }
  get metallic() { return this.uniforms.uMetallic.value; }
  set metallic(v) { this.uniforms.uMetallic.value = v; }
  get noMetallic() { return this.uniforms.uNoMetallic.value; }
  set noMetallic(v) { this.uniforms.uNoMetallic.value = v; }
  get intensity() { return this.uniforms.uIntensity.value; } // face SDF 强度
  set intensity(v) { this.uniforms.uIntensity.value = v; }
  get shadowColor() { return this.uniforms.uShadowColor.value; }
  set shadowColor(v) { this.uniforms.uShadowColor.value.set(v); }
  get emissiveIntensity() { return this.uniforms.uEmissiveIntensity.value; }
  set emissiveIntensity(v) { this.uniforms.uEmissiveIntensity.value = v; }
  /* 注意：不能用 setter 名 `toneMapped`——与 three Material 基类 constructor 的
     this.toneMapped = true 撞车（super() 阶段 uniforms 尚未创建，setter 必炸） */
  setToneMapped(v) { this.uniforms.uToneMapped.value = v; }
  getToneMapped() { return this.uniforms.uToneMapped.value; }
  get forwardVec() { return this.uniforms.uForwardVec.value; }
  set forwardVec(v) { this.uniforms.uForwardVec.value.copy(v); }
  get rightVec() { return this.uniforms.uRightVec.value; }
  set rightVec(v) { this.uniforms.uRightVec.value.copy(v); }
  get outlineWidth() { return this.uniforms.uOutLineWidth.value; }
  set outlineWidth(v) { this.uniforms.uOutLineWidth.value = v; }

  /* ---- 静态装配：traverse glTF 场景按 mat.name 分派 + 附加描边壳 ---- */
  static applyToSkeleton(root, options = {}) {
    const tex = options.textures || {};
    const shared = GenshinToonMaterial.ensureShared(options);
    const materials = [];
    const outlineMaterials = [];

    // 整树克隆做描边壳（KallkaGo 同款：独立 outlineModel + scale 1.0001）；
    // 克隆树 add 进 root，旋转/位移自动同步（三视角轮播只转 root）
    const outlineRoot = root.clone(true);
    const mainMeshes = [];
    root.traverse((o) => { if (o.isMesh) mainMeshes.push(o); });
    const outlineMeshes = [];
    outlineRoot.traverse((o) => { if (o.isMesh) outlineMeshes.push(o); });

    mainMeshes.forEach((mesh, i) => {
      const geo = mesh.geometry;
      const srcMat = mesh.material; // glTF 来的 MeshStandardMaterial
      const name = (srcMat.name || '').toLowerCase();
      const isFace = name === 'face';
      const isHair = name === 'hair' || name === 'dress';

      // 贴图组（Ayaka.tsx 286/321/327 行实证）：face→face 贴图组；hair/dress→hair 组；其余→body 组
      const mat = new GenshinToonMaterial({
        isFace,
        name: 'toon-' + (name != null ? name : 'body'),
        baseMap: srcMat.map || null, // glb 内嵌贴图优先
        baseColor: srcMat.color,
        transparent: srcMat.transparent,
        side: srcMat.side,
        depthWrite: srcMat.depthWrite,
        depthTest: srcMat.depthTest,
        // geometry 缺 TANGENT 时 three prefix 不注入声明，shader 走退化 define
        defines: geo.attributes.tangent ? {} : { CW_NO_TANGENT: 1 },
        lightMap: isFace ? null : (isHair ? tex.hairLightMap : tex.bodyLightMap),
        rampMap: tex.bodyRampMap, // face 也用 bodyRamp（Ayaka 299 行）
        normalMap: isFace ? null : (isHair ? tex.hairNormalMap : tex.bodyNormalMap),
        emissiveMap: isFace || isHair ? null : tex.emissiveMap, // hair 组 emissive null（Ayaka 325）
        metalMap: tex.metalMap,
        faceSDFMap: isFace ? tex.faceLightMap : null,
      });
      mesh.material = mat;
      materials.push(mat);

      // 描边壳 mesh（BackSide + vertexColors + 外扩）：逐 mesh 检测 geometry
      // 是否具备 _uv7 / color / tangent，缺啥注入对应退化 define，不崩
      const oMat = new GenshinOutlineMaterial({
        name: 'outline-' + (name || 'body'),
        baseMap: srcMat.map || null, // 描边底色同源 glb 内嵌贴图（Ayaka 347）
        baseColor: srcMat.color,
        hasUv7: !!geo.attributes._uv7,
        hasColor: !!geo.attributes.color,
        noTangent: !geo.attributes.tangent,
      });
      const oMesh = outlineMeshes[i];
      oMesh.material = oMat;
      outlineMaterials.push(oMat);
    });

    outlineRoot.scale.setScalar(1.0001); // 外壳微放大防 z-fighting（KallkaGo 同款）
    root.add(outlineRoot);

    return { materials, outlineMaterials, outlineRoot, shared };
  }

  /* ---- 深度 provider：body.frag 深度偏移 rim 需要 scene depth texture ----
     照 useDepthTexture.ts：overrideMaterial=MeshDepthMaterial、隐藏描边壳、
     渲完还原。不接则 rim 归零退化（uDepthTexture 兜底 1x1 白纹理）不崩。
     pre-pass 帧序：depth -> outline -> 主 pass（页面 rAF 先调 prePass() 再 render） */
  static attachDepthProvider(renderer, scene, camera, ignoreList = []) {
    const shared = GenshinToonMaterial.ensureShared();
    const bufSize = new THREE.Vector2();
    renderer.getDrawingBufferSize(bufSize);
    const rt = new THREE.WebGLRenderTarget(bufSize.x, bufSize.y, {
      depthBuffer: true,
      stencilBuffer: false,
      depthTexture: new THREE.DepthTexture(bufSize.x, bufSize.y),
    });
    rt.depthTexture.format = THREE.DepthFormat;
    rt.depthTexture.type = THREE.UnsignedShortType;
    const depthMat = new THREE.MeshDepthMaterial({ side: THREE.DoubleSide });
    const bgColor = new THREE.Color(0x000000);
    shared.uNear.value = camera.near;
    shared.uFar.value = camera.far;
    // 深度纹理灌进全部已存在的卡通材质（描边材质无 uDepthTexture，instanceof 过滤）
    GenshinToonMaterial.#applyToAll((m) => {
      if (m instanceof GenshinToonMaterial) m.uniforms.uDepthTexture.value = rt.depthTexture;
    });
    const api = {
      depthTexture: rt.depthTexture,
      prePass() {
        renderer.getDrawingBufferSize(bufSize);
        /* 与 useDepthTexture.ts 同款：手动同步 depthTexture 尺寸再 setSize */
        rt.depthTexture.image.width = bufSize.x;
        rt.depthTexture.image.height = bufSize.y;
        rt.setSize(bufSize.x, bufSize.y);
        const originalBg = scene.background;
        scene.background ??= bgColor;
        scene.overrideMaterial = depthMat;
        ignoreList.forEach((obj) => { obj.visible = false; }); // 描边壳不进 depth（ignoreList 同款）
        renderer.setRenderTarget(rt);
        renderer.render(scene, camera);
        renderer.setRenderTarget(null);
        scene.overrideMaterial = null;
        scene.background = originalBg;
        ignoreList.forEach((obj) => { obj.visible = true; });
      },
      dispose() {
        rt.depthTexture.dispose();
        rt.dispose();
        depthMat.dispose();
      },
    };
    return api;
  }

  /* 内部：遍历当前已创建的全部卡通材质（含描边材质外的）做批量操作 */
  static #applyToAll(fn) {
    // 静态私有字段只存共享表；材质遍历通过场景遍历做不到（无 scene 引用），
    // 改由 applyToSkeleton 返回值驱动——这里用注册表兜底
    for (const m of GenshinToonMaterial.#registry) fn(m);
  }
  static #registry = new Set();
  /* 注册/注销（constructor/dispose 时自动） */
  static _register(m) { GenshinToonMaterial.#registry.add(m); }
  static _unregister(m) { GenshinToonMaterial.#registry.delete(m); }
}

/* ---------- 描边材质：BackSide + vertexColors + 独立贴图（同文件，KallkaGo outlineModel 同款） ---------- */

export class GenshinOutlineMaterial extends THREE.ShaderMaterial {
  constructor(options = {}) {
    const shared = GenshinToonMaterial.ensureShared();
    const defines = {};
    // geometry 无 _uv7 时不定义 -> shader 走普通法线退化路径；
    // 无 TANGENT 时 _uv7 的 TBN 变换没有基，一并退化（与主材质同判据）
    if (options.hasUv7 && !options.noTangent) defines.CW_HAS_UV7 = 1;
    if (options.noTangent) defines.CW_NO_TANGENT = 1;
    const c = options.baseColor || new THREE.Color(1, 1, 1);
    super({
      name: options.name || 'GenshinOutline',
      defines,
      uniforms: {
        uBaseMap: { value: prepBase(options.baseMap || null) },
        uBaseColor: { value: new THREE.Vector4(c.r, c.g, c.b, 1) },
        uOutLineWidth: shared.uOutLineWidth,
        uResolution: shared.uResolution,
      },
      vertexShader: OUTLINE_VERTEX,
      fragmentShader: OUTLINE_FRAG,
      side: THREE.BackSide,
      vertexColors: options.hasColor !== false, // 无顶点色时关掉注入 + shader 内 CW_NO_VCOLOR 由 JS define 补
      // 无顶点色：shader 的 cwColorAlpha() 走 CW_NO_VCOLOR 分支（define 缺省时
      // USE_COLOR_ALPHA/USE_COLOR 都未定义，同样返回 1.0，天然退化，无需额外 define）
    });
    GenshinToonMaterial._register(this);
    this.addEventListener('dispose', () => GenshinToonMaterial._unregister(this));
  }
  get outlineWidth() { return this.uniforms.uOutLineWidth.value; }
  set outlineWidth(v) { this.uniforms.uOutLineWidth.value = v; }
}
