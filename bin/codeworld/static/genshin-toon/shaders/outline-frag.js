// Based on KallkaGo/ToonShading (MIT) — Copyright (c) 2024 KallkaGo
// 描边 fragment：底色压暗 15%。原版 csm_FragColor 是 CSM 在 three
// tonemapping/colorspace 之后的末位覆写——所以这里同样不做 tone map / sRGB
// 编码，直接出屏（与 KallkaGo 输出语义一致：描边恒暗色）。
// 去 CSM 化：csm_DiffuseColor -> uBaseMap 采样 × uBaseColor。

export const OUTLINE_FRAG = /* glsl */ `
varying float uOpacity;
varying vec2 vUv;
uniform sampler2D uBaseMap;
uniform vec4 uBaseColor;

void main() {
  vec4 baseColor = texture2D(uBaseMap, vUv) * uBaseColor;
  if(baseColor.a < 0.5)
    discard;
  gl_FragColor = vec4(baseColor.rgb * .15, uOpacity);
}
`;
