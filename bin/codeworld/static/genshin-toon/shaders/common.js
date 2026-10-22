// Based on KallkaGo/ToonShading (MIT) — Copyright (c) 2024 KallkaGo
// 公共 GLSL 片段：所有 fragment 共用的输出链与数学工具。
//
// 【去 CSM 化的关键决策】原版把卡通光照结果写进 csm_Emissive，由挂载的
// MeshStandardMaterial 走 three 内置收尾：ambient 间接光贡献 → ACES Filmic
// tone mapping → sRGB 编码。独立 ShaderMaterial 没有这条内置链，这里手工
// 复刻（r160 同款公式），保证与 KallkaGo demo 输出色调一致。

export const GLSL_COMMON = /* glsl */ `
#define PI 3.141592653589793

/* GT tone map 后处理复刻（KallkaGo GTToneMap 效果，参数=Sketch.tsx leva 底账）：
   KallkaGo 链 = 材质输出（无 renderer tone mapping，ThreeContainer gl.NoToneMapping）
   -> sRGB 编码 -> bloom -> GT tone map 作用在已编码值上 -> 出屏。
   本材质 = sRGB 编码 -> GT（bloom 缺失，差异源清单） */
uniform float uGTMaxLum;
uniform float uGTContrast;
uniform float uGTLinearStart;
uniform float uGTLinearLen;
uniform float uGTBlackC;
uniform float uGTBlackB;
uniform float uGTEnabled;

/* ---- three r160 packing.glsl 内联（body.frag 的深度 rim 需要）---- */
float toonPerspectiveDepthToViewZ(float invClipZ, float near, float far) {
  return (near * far) / ((far - near) * invClipZ - far);
}
float toonViewZToOrthographicDepth(float viewZ, float near, float far) {
  return (viewZ + near) / (far - near);
}

/* ---- ACES Filmic tone mapping（three r160 同款；默认关，构造参数可开）---- */
const mat3 CW_ACES_INPUT = mat3(
  vec3(0.59719, 0.07600, 0.02840),
  vec3(0.35458, 0.90834, 0.13383),
  vec3(0.04823, 0.01566, 0.83777)
);
const mat3 CW_ACES_OUTPUT = mat3(
  vec3( 1.60475, -0.10208, -0.00327),
  vec3(-0.53108,  1.10813, -0.07276),
  vec3(-0.07367, -0.00605,  1.07602)
);
vec3 toonACES(vec3 color) {
  color *= 1.0 / 0.6;
  color = CW_ACES_INPUT * color;
  color = clamp((color * (color + 0.0245786) - 0.000090537) /
                (color * (0.983729 * color + 0.4329510) + 0.238081), 0.0, 1.0);
  color = CW_ACES_OUTPUT * color;
  return clamp(color, 0.0, 1.0);
}

/* ---- linear -> sRGB 编码（three r160 colorspace_fragment 同款 OETF）---- */
vec3 toonSRGB(vec3 c) {
  vec3 s = mix(c * 12.92, 1.055 * pow(c, vec3(1.0 / 2.4)) - 0.055, step(vec3(0.0031308), c));
  return s;
}

/* Gran Turismo tonemapper（KallkaGo toneMap/fragment.glsl 同款，向量化） */
vec3 toonGTToneMap(vec3 x) {
  float P = uGTMaxLum;
  float a = uGTContrast;
  float m = uGTLinearStart;
  float l = uGTLinearLen;
  float c = uGTBlackC;
  float b = uGTBlackB;
  float l0 = (P - m) * l / a;
  float S0 = m + l0;
  float S1 = m + a * l0;
  float C2 = a * P / (P - S1);
  vec3 L_x = m + a * (x - m);
  vec3 T_x = m * pow(max(x, 0.0) / m, vec3(c)) + b;
  vec3 S_x = P - (P - S1) * exp(-(C2 * (x - S0) / P));
  /* W_f(x,0,m) 与 smoothstep 同形；H_f(e,e) 与 step 同形 */
  vec3 w0 = vec3(1.0) - smoothstep(vec3(0.0), vec3(m), x);
  vec3 w2 = step(vec3(m + l0), x);
  vec3 w1 = vec3(1.0) - w0 - w2;
  return T_x * w0 + L_x * w1 + S_x * w2;
}

/* ---- 统一收尾：ambient 间接漫反射 + 卡通光照 -> [ACES 可选] -> sRGB -> [GT 可选] ----
   uAmbientColor 在 JS 侧已预除 PI（BRDF_Lambert = diffuseColor/PI），
   与 KallkaGo 版 MeshStandardMaterial 的 ambient 间接光贡献等价 */
vec4 toonFinalColor(vec3 albedo, vec4 baseColor) {
  vec3 outgoing = albedo + uAmbientColor * baseColor.rgb;
  if (uToneMapped > 0.5) outgoing = toonACES(outgoing);
  outgoing = toonSRGB(outgoing);
  outgoing = mix(outgoing, toonGTToneMap(outgoing), uGTEnabled);
  return vec4(outgoing, baseColor.a);
}
`;
