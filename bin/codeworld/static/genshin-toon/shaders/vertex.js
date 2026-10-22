// Based on KallkaGo/ToonShading (MIT) — Copyright (c) 2024 KallkaGo
// 共享顶点着色器：世界法线/切线/副切线/视线方向/屏幕坐标（body+face 共用）。
// 去 CSM 化：原版依赖 CSM 注入 three 顶点尾自动写 gl_Position，独立版手动补。

export const TOON_VERTEX = /* glsl */ `
/* 骨骼蒙皮：SkinnedMesh 的 USE_SKINNING 由 three 自动开启（按 object 判定），
   但 ShaderMaterial 不会自动应用骨骼变换——这里接 three 内置 chunk 链
   （beginnormal/skinbase/skinnormal/begin_vertex/skinning_vertex），
   否则渲出的是 bind-pose 顶点（T-pose），与参考 demo 的 rest pose 不一致 */
#include <common>
#include <skinning_pars_vertex>

varying vec2 vUv;
varying vec3 vWorldNormal;
varying vec3 vWorldTangent;
varying vec3 vWorldBitangent;
varying vec3 vDirWs;
varying vec4 vScreenPos;
varying mat4 vViewMatrix;

#ifdef CW_NO_TANGENT
/* geometry 无 TANGENT 属性：退化兜底（JS 侧检测后注入此 define） */
vec4 CW_TANGENT_ATTR = vec4(0.0, 0.0, 1.0, 1.0);
#define CW_TANGENT CW_TANGENT_ATTR
#else
/* three 只在内置 normalMap 语义下注入 attribute vec4 tangent 声明
   （vertexTangents = geometry.tangent && material.normalMap），自定义
   ShaderMaterial 恒不满足 -> 这里自行声明（本材质不设 normalMap，永不双声明） */
attribute vec4 tangent;
#define CW_TANGENT tangent
#endif

vec4 ComputeScreenPos(vec4 pos) {
  vec4 o = pos * 0.5;
  o.xy = vec2(o.x, o.y) + o.w;
  o.zw = pos.zw;
  return o;
}

void main() {
  vUv = uv;
  /* 骨骼变换链：objectNormal/transformed 由 three chunk 定义并应用蒙皮 */
  #include <beginnormal_vertex>
  #include <skinbase_vertex>
  #include <skinnormal_vertex>
  #include <begin_vertex>
  #include <skinning_vertex>

  vWorldNormal = (modelMatrix * vec4(objectNormal, 0.0)).xyz;
  vec3 transTangent = (modelMatrix * vec4(CW_TANGENT.xyz, 0.0)).xyz;
  vWorldTangent = normalize(transTangent);
  vWorldBitangent = normalize(cross(vWorldNormal, vWorldTangent) * CW_TANGENT.w);
  vec4 Ws = modelMatrix * vec4(transformed, 1.0);
  vDirWs = normalize(cameraPosition - Ws.xyz);
  vec4 clipPos = projectionMatrix * modelViewMatrix * vec4(transformed, 1.0);
  /* 独立 ShaderMaterial：three 不会替我们写 gl_Position，手动补上（原 csm_PositionRaw 通道） */
  gl_Position = clipPos;
  vScreenPos = ComputeScreenPos(clipPos);
  vViewMatrix = viewMatrix;
}
`;
