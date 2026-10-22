// Based on KallkaGo/ToonShading (MIT) — Copyright (c) 2024 KallkaGo
// 描边顶点着色器：背面外壳法线外扩（NDC 空间偏移），顶点色 alpha 控顶点级描边宽。
// 去 CSM 化：csm_PositionRaw -> gl_Position；_uv7 平均法线带退化路径
//（geometry 无 _uv7 属性时由 JS 注入 CW_HAS_UV7 的反面分支，用普通法线替代）。

export const OUTLINE_VERTEX = /* glsl */ `
/* 骨骼蒙皮：同主材质（ShaderMaterial 不自动应用骨骼变换，接 three chunk 链） */
#include <common>
#include <skinning_pars_vertex>

#ifdef CW_HAS_UV7
attribute vec3 _uv7; /* 平均法线（建模烘焙），自定义 attribute 必须自声明 */
#ifndef CW_NO_TANGENT
/* three 只在内置 normalMap 语义下注入 tangent 声明，自定义 ShaderMaterial
   恒不满足 -> 自行声明（geometry 无 TANGENT 时 JS 注入 CW_NO_TANGENT 不进此分支） */
attribute vec4 tangent;
#endif
#endif
uniform float uOutLineWidth;
uniform vec2 uResolution;
varying vec3 vNor;
varying vec2 vUv;
varying float uOpacity;

/* 顶点色 alpha（three prefixVertex 按 geometry 自动声明 color attribute：
   vertexColors + itemSize4 -> USE_COLOR_ALPHA -> attribute vec4 color；
   geometry 无顶点色时 JS 侧注入 CW_NO_VCOLOR，宽度系数退化为 1） */
float cwColorAlpha() {
  #ifdef CW_NO_VCOLOR
  return 1.0;
  #else
    #ifdef USE_COLOR_ALPHA
  return color.a;
    #else
  return 1.0;
    #endif
  #endif
}

void main() {
  #if defined(CW_HAS_UV7) && !defined(CW_NO_TANGENT)
  /* _uv7 平均法线经 TBN 变换到世界方向（需要 TANGENT 属性，已自行声明） */
  vec3 tansTangent = normalize(tangent.xyz);
  vec3 bitangent = normalize(cross(normal, tansTangent) * tangent.w);
  mat3 tbn = mat3(tansTangent, bitangent, normal);
  vec3 aveNormal = normalize(tbn * _uv7);
  #else
  /* 退化路径：无 _uv7 或无 TANGENT 时用普通法线替代（描边质量略降但不崩） */
  vec3 aveNormal = normalize(normal);
  #endif
  /* 骨骼变换链：transformed 由 three chunk 定义并应用蒙皮 */
  #include <beginnormal_vertex>
  #include <skinbase_vertex>
  #include <skinnormal_vertex>
  #include <begin_vertex>
  #include <skinning_vertex>
  vec4 clipPosition = projectionMatrix * modelViewMatrix * vec4(transformed, 1.0);
  /* 描边外扩方向照原版用 _uv7 平均法线（模型空间），蒙皮后顶点位置照常变换 */
  vec3 viewNormal = normalize(normalMatrix * aveNormal);
  vec4 clipNormal = projectionMatrix * vec4(viewNormal, 0.0);
  vec3 ndcNormal = clipNormal.xyz;
  float aspect = abs(uResolution.y / uResolution.x);
  clipNormal.x *= aspect;
  /* in Opengl clip space w = -zView  */
  float ctrlCSw = clamp(clipPosition.w, .5, 3.);
  clipPosition.xy += 0.01 * uOutLineWidth * ndcNormal.xy * cwColorAlpha() * ctrlCSw;
  // clipPosition.z += 0.0001 * ndcNormal.z;
  uOpacity = step(.001, uOutLineWidth);
  /* 原 csm_PositionRaw 通道 -> gl_Position */
  gl_Position = clipPosition;
  vNor = aveNormal;
  vUv = uv;
}
`;
