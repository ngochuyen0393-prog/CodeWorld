// Based on KallkaGo/ToonShading (MIT) — Copyright (c) 2024 KallkaGo
// 身体 fragment：ILM 分区光照 + ramp 双色阶（日/夜）+ 金属 matcap + 深度偏移 rim + fresnel。
// 去 CSM 化：csm_DiffuseColor -> uBaseMap 采样 × uBaseColor；
// csm_Emissive -> toonFinalColor（ambient + ACES + sRGB，见 common.js）。

import { GLSL_COMMON } from './common.js';

export const TOON_BODY_FRAG = /* glsl */ `
varying vec2 vUv;
varying vec3 vWorldNormal;
varying vec3 vWorldTangent;
varying vec3 vWorldBitangent;
varying vec3 vDirWs;
uniform vec3 uLightPosition;
varying vec4 vScreenPos;
varying mat4 vViewMatrix;
uniform sampler2D uLightMap;
uniform sampler2D uRampMap;
uniform sampler2D uMetalMap;
uniform sampler2D uNormalMap;
uniform sampler2D uEmissiveMap;
uniform sampler2D uDepthTexture;
uniform float uIsDay;
uniform vec3 uShadowColor;
uniform float uNoMetallic;
uniform float uMetallic;
uniform float uTime;
uniform float uRimLightWidth;
uniform float uRimLightIntensity;
/* D-004c rim 泛化：独立颜色/背光压暗/分区宽度开关 + 阈值软度参数化（默认值=旧硬编码 0.12/1.0，回归安全） */
uniform float uRimThreshold;
uniform float uRimFadeout;
uniform vec3 uRimColor;
uniform float uRimShadeMix;
uniform float uRimByMatId;
/* D-004c 头发天使光圈（KKK 移位切线各向异性高光）：公式=教科书 Kajiya-Kay 变体自实现；
   「分区乘 mask 组织」思路参考 MIT Gaolingx/GenshinCelShaderURP 的
   AvatarSpecularHelper.specular_color，未直译其代码。仅 hair 变体（CW_HAIR_ANISO）编译本段 */
#ifdef CW_HAIR_ANISO
uniform float uAnisoShift;
uniform float uAnisoShininess;
uniform float uAnisoIntensity;
uniform float uAnisoSecondary;
uniform float uAnisoSecondaryShift;
uniform vec3 uAnisoColor;
#endif
uniform float uNear;
uniform float uFar;
uniform float uIntensity;
uniform float uEmissiveIntensity;
/* 去 diffuseColor 依赖：baseMap 采样 + 底色（glb 材质 baseColorFactor） */
uniform sampler2D uBaseMap;
uniform vec4 uBaseColor;
/* 统一输出链 uniforms（common.js toonFinalColor 用） */
uniform vec3 uAmbientColor;
uniform float uToneMapped;

${GLSL_COMMON}

float RampMapRow0 = 1.;
float RampMapRow1 = 4.;
float RampMapRow2 = 3.;
float RampMapRow3 = 5.;
float RampMapRow4 = 2.;

float readDepth(sampler2D depthSampler, vec2 coord) {
  float fragCoordZ = texture2D(depthSampler, coord).x;
  /* three r160 packing.glsl 两个内置函数已内联进 common.js（去 three chunk 依赖） */
  float viewZ = toonPerspectiveDepthToViewZ(fragCoordZ, uNear, uFar);
  return toonViewZToOrthographicDepth(viewZ, uNear, uFar);
}

/* D-004c：KKK 移位切线各向异性高光条带（教科书公式自写）：
   shiftedT = normalize(T + shift·N)，H = normalize(L + V)，
   band = pow(sqrt(1 - dot(shiftedT,H)²), shininess)（Kajiya-Kay sin 展开；
   sqrt 内 max 钳位防浮点误差出 NaN），条带沿发丝切线走向铺开 */
vec3 cwAnisoBand(vec3 n, vec3 t, vec3 hDir, float shift, float shininess, float intensity, vec3 color) {
  vec3 shiftedT = normalize(t + shift * n);
  float dotSH = dot(shiftedT, hDir);
  float band = pow(sqrt(max(1.0 - dotSH * dotSH, 0.0)), shininess);
  return color * band * intensity;
}

/* D-004c：ILM .a 材质分区的 rim 宽度系数（自定底账 0.7→1.3 升序五档：亮面
   材质区收窄防糊边缘、暗面布料区放宽增强轮廓可读性；分区边界沿用 ramp 选择
   的同款中点 0.15/0.40/0.60/0.85。uRimByMatId=0 时不影响输出） */
float cwRimZoneMod(float a) {
  float w = 0.7;
  w = mix(w, 0.85, step(0.15, a));
  w = mix(w, 1.0, step(0.40, a));
  w = mix(w, 1.15, step(0.60, a));
  w = mix(w, 1.3, step(0.85, a));
  return w;
}

void main() {
  /* 处理需要的数据 */

  /* normalMap */
  vec4 normalTex = texture2D(uNormalMap, vUv);
  vec3 normalTs = vec3(normalTex.rg * 2. - 1., 0.);
  normalTs.z = sqrt(1. - dot(normalTs.xy, normalTs.xy));

  mat3 tbn = mat3(normalize(vWorldTangent), normalize(vWorldBitangent), normalize(vWorldNormal));

  vec3 worldNormal = normalize(tbn * normalTs);
  vec3 viewNormal = (vViewMatrix * vec4(worldNormal, 0.)).xyz;

  /* lightMap（D-004c 上移到屏幕偏移之前：分区 rim 宽度调制要先拿到 .a 材质分区） */
  vec4 lightMapTex = texture2D(uLightMap, vUv);

  /* Screen Pos（rim 深度偏移采样点；D-004c 宽度乘分区系数，uRimByMatId=0 时系数恒 1=旧行为） */
  vec2 scrPos = vScreenPos.xy / vScreenPos.w;
  float rimWidthMod = mix(1., cwRimZoneMod(lightMapTex.a), uRimByMatId);
  vec2 scrOffsetPos = scrPos + viewNormal.xy * uRimLightWidth * rimWidthMod * 0.01;

  vec3 dirL = normalize(uLightPosition);
  vec3 hDirWS = normalize(vDirWs + dirL);

  vec2 matcapUV = (viewNormal.xy + 1.) * .5;

  float NDotL = dot(worldNormal, dirL); //lambert

  float lambert = max(NDotL, 0.);

  float NDotH = dot(worldNormal, hDirWS); //Blinn-Phong

  float NdotV = dot(worldNormal, vDirWs); //fresnel

  float halfLambert = pow(lambert * .5 + .5, 2.);
  float lamberStep = smoothstep(.42, .45, halfLambert);

  /* 枚举样条阈值 */
  float matEnum0 = .0;
  float matEnum1 = .3;
  float matEnum2 = .5;
  float matEnum3 = .7;
  float matEnum4 = 1.;

  /* 计算每一行样条的中心点 */
  float ramp0 = RampMapRow0 / 10. - .05;
  float ramp1 = RampMapRow1 / 10. - .05;
  float ramp2 = RampMapRow2 / 10. - .05;
  float ramp3 = RampMapRow3 / 10. - .05;
  float ramp4 = RampMapRow4 / 10. - .05;

  /* 根据魔法图alpha通道的阈值 计算rampV */
  float dayRampV = mix(ramp4, ramp3, step(lightMapTex.a, (matEnum4 + matEnum3) * .5));
  dayRampV = mix(dayRampV, ramp2, step(lightMapTex.a, (matEnum3 + matEnum2) * .5));
  dayRampV = mix(dayRampV, ramp1, step(lightMapTex.a, (matEnum2 + matEnum1) * .5));
  dayRampV = mix(dayRampV, ramp0, step(lightMapTex.a, (matEnum1 + matEnum0) * .5));

  float nightRampV = dayRampV + .5;

  float rampClampMin = .003;
  float rampClampMax = .997;

  /* 防止取到边界 */
  float rampU = clamp(smoothstep(.2, .4, halfLambert), rampClampMin, rampClampMax);
  vec2 dayUV = vec2(rampU, 1. - dayRampV);
  vec2 nightUV = vec2(rampU, 1. - nightRampV);

  vec2 darkDayUV = vec2(rampClampMin, 1. - dayRampV);
  vec2 darkNightUV = vec2(rampClampMin, 1. - nightRampV);

  float uIsDay = (uIsDay + 1.) * .5;
  vec3 rampGreyColor = mix(texture2D(uRampMap, nightUV).rgb, texture2D(uRampMap, dayUV).rgb, uIsDay);
  vec3 rampDarkColor = mix(texture2D(uRampMap, darkNightUV).rgb, texture2D(uRampMap, darkDayUV).rgb, uIsDay);

  /* 原 csm_DiffuseColor：baseMap（SRGB 纹理硬件转 linear）× 材质底色 */
  vec4 baseColor = texture2D(uBaseMap, vUv) * uBaseColor;

  vec3 grayShadowColor = baseColor.rgb * rampGreyColor * uShadowColor;
  vec3 darkShadowColor = baseColor.rgb * rampDarkColor * uShadowColor;

  /* light.g > 0.5的部分受光照影响 */
  vec3 diffuse = vec3(0.);
  diffuse = mix(grayShadowColor, baseColor.rgb, lamberStep);
  // 处理0-0.5的部分 常暗
  diffuse = mix(darkShadowColor, diffuse, clamp(lightMapTex.g * 2., 0., 1.));
  // 处理0.5-1的部分 受光照影响
  diffuse = mix(diffuse, baseColor.rgb, clamp((lightMapTex.g - .5), 0., 1.) * 2.);

  float blinPhong = step(0., NDotL) * pow(max(NDotH, 0.), 10.);
  /* 避免漏光 */
  vec3 noMetalicSpec = vec3(step(1.04 - blinPhong, lightMapTex.b) * lightMapTex.r) * uNoMetallic;

  /*根据光的方向有一个衰减 这里设置的下限是0.2 ，光滑的非金属表面是不吸收颜色的，但是金属会吸收 所以需要乘以baseColor */
  vec3 metalicSpec = vec3(blinPhong * lightMapTex.b * (lamberStep * .8 + .2) * baseColor.rgb) * uMetallic;

  /* 根据魔法图r通道提取金属区域 */
  float isMetal = step(.95, lightMapTex.r);

  vec3 finalSpec = mix(noMetalicSpec, metalicSpec, isMetal);

  vec3 metallic = mix(vec3(0.), texture2D(uMetalMap, matcapUV).rgb * baseColor.rgb, isMetal);

  float fresnel = 1. - clamp(NdotV, 0., 1.);

  float fresnelClamp = .5;

  fresnel = pow(fresnel, 8.);

  fresnel = fresnel * fresnelClamp + (1. - fresnelClamp);

  /* 边缘光（深度偏移 rim：需要 scene depth texture）。
     D-004c 泛化①：独立颜色 uRimColor 替换原 diffuse 因子（默认白最接近旧观感；
     旧版暗部轮廓 rim 随底色变暗、白 rim 不随——亮度分布差异已入差异源清单）。
     ②阈值/软度参数化（默认值=旧硬编码 0.12/1.0，回归安全） */
  float offsetDepth = readDepth(uDepthTexture, scrOffsetPos);
  float currentDepth = readDepth(uDepthTexture, scrPos);
  float depthDiff = clamp(offsetDepth - currentDepth, 0., 1.);
  float rimIntensity = smoothstep(uRimThreshold, uRimFadeout, depthDiff);
  vec3 rimLight = uRimColor * rimIntensity * uRimLightIntensity * fresnel;
  /* D-004c 泛化③背光 rim 压暗：背光面（lamberStep→0）把 rim 压到 0.3 档，
     受光面边缘不受影响（「rim 随光照方向衰减」思想出处见
     LICENSE-THIRD-PARTY.md 的 D-004c 声明节，实现自写） */
  rimLight = mix(rimLight, rimLight * 0.3, uRimShadeMix * (1. - lamberStep));

  /* 自发光 */
  vec4 emissiveTex = texture2D(uEmissiveMap, vUv);
  emissiveTex.a = smoothstep(0., 1., emissiveTex.a);

  vec3 glow = mix(vec3(0.), emissiveTex.rgb * abs(sin(uTime * .8)) * uEmissiveIntensity, emissiveTex.a);

  /* D-004c 头发天使光圈：主条带 + 可选副条带（第二条带用不同 shift 的切线
     再算一遍同公式，默认强度 0=关）。双约束：lamberStep 背光面不出圈 +
     ILM .g 受光区 mask（实现见下方 mask 实测校准注释）。
     半角向量复用上方 hDirWS；切线用顶点级世界切线（发丝走向） */
  vec3 anisoSpec = vec3(0.);
  #ifdef CW_HAIR_ANISO
  anisoSpec = cwAnisoBand(worldNormal, normalize(vWorldTangent), hDirWS,
                          uAnisoShift, uAnisoShininess, uAnisoIntensity, uAnisoColor);
  if (uAnisoSecondary > 0.0) {
    anisoSpec += cwAnisoBand(worldNormal, normalize(vWorldTangent), hDirWS,
                             uAnisoSecondaryShift, uAnisoShininess, uAnisoSecondary, uAnisoColor);
  }
  /* mask 实测校准：hair/light.png 的 .g 双峰分布（≈0.5=受光主体 51.8%、≈0=常暗 41.3%），
     smoothstep(.25,.5) 把受光主体拉满、常暗区归零——不用 (g-.5)*2 是因为它把
     标称值 0.5 的主体区域全灭（首版实测条带不可见的根因） */
  anisoSpec *= lamberStep * smoothstep(.25, .5, lightMapTex.g);
  #endif

  vec3 albedo = diffuse + finalSpec + metallic + glow + rimLight + anisoSpec;

  if(baseColor.a < .5) {
    discard;
  }

  /* 原 csm_Emissive 通道：经统一输出链（ambient + ACES + sRGB）出屏 */
  gl_FragColor = toonFinalColor(albedo, baseColor);
}
`;
