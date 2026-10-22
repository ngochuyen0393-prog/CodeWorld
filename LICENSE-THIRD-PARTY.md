# 第三方组件声明（LICENSE-THIRD-PARTY）

本仓库包含以下第三方组件，按其原始许可证使用：

| 组件 | 许可证 | 位置 | 上游 | 用途 |
|------|--------|------|------|------|
| ToonShading 卡通渲染 shader | MIT | `bin/codeworld/static/genshin-toon/` | [KallkaGo/ToonShading](https://github.com/KallkaGo/ToonShading)（Copyright (c) 2024 KallkaGo） | 原神式卡通渲染材质（GenshinToonMaterial） |

## KallkaGo/ToonShading (MIT)

Copyright (c) 2024 KallkaGo

本仓库 `bin/codeworld/static/genshin-toon/` 下的 GLSL 移植（body/face/outline
shader 与 GenshinToonMaterial.js）基于 KallkaGo/ToonShading 项目的 GLSL 管线
移植（去 CSM 化：从 three-custom-shader-material 注入式实现抽为独立
ShaderMaterial），每个移植文件头已注明出处。

MIT License 全文（英文原文为准）：

> Permission is hereby granted, free of charge, to any person obtaining a copy
> of this software and associated documentation files (the "Software"), to deal
> in the Software without restriction, including without limitation the rights
> to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
> copies of the Software, and to permit persons to whom the Software is
> furnished to do so, subject to the following conditions:
>
> The above copyright notice and this permission notice shall be included in all
> copies or substantial portions of the Software.
>
> THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
> IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
> FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
> AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
> LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
> OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE OTHER DEALINGS IN THE
> SOFTWARE.

## D-004c 天使光圈（aniso 高光）与 rim 泛化公式来源声明

- **头发天使光圈（各向异性高光）**：教科书 Kajiya-Kay 移位切线（shifted
  tangent）各向异性高光的 sin 展开（`pow(sqrt(1-dot(shiftedT,H)²), shininess)`），
  **自实现**，未直译任何上游代码。「分区乘 mask」（specColor × 光照图通道 mask
  的组织方式）**思路**参考 MIT 许可的
  [Gaolingx/GenshinCelShaderURP](https://github.com/Gaolingx/GenshinCelShaderURP)
  （`AvatarSpecularHelper.hlsl` 的 `specular_color()`），未复制其代码。
- **rim 泛化（独立 rim 颜色 / 背光压暗 / ILM 分区 rim 宽度）**：实现全自写
  （沿用本仓库已有的 KallkaGo 深度偏移 rim 骨架）；「rim 颜色独立分区」与
  「rim 随光照方向衰减（背光压暗）」两个**思想**参考 GPL-3.0 的
  [stalomeow/StarRailNPRShader](https://github.com/stalomeow/StarRailNPRShader)
  ——仅借鉴思想，未复制其任何代码、命名、结构或常量序列（自查见 D-004c
  任务报告的 GPL 红线自查节）。

## miHoYo 测试资产声明（重要）

`bin/codeworld/static/models/ayakaktx2opt.glb`（神里绫华模型）与
`bin/codeworld/static/textures/` 下的全部贴图均为 **miHoYo（米哈游）版权资产**，
来源为 KallkaGo/ToonShading 仓库自带的测试资源。它们**仅限本地渲染测试使用**，
**禁止再分发、禁止商用**。模型文件不入 git（见 `bin/codeworld/static/models/README.md`）；
贴图按任务定案随仓库走私有存储（同样仅本地测试语义）。任何对外发布前必须
先移除这些资产。
