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

## miHoYo 测试资产声明（重要）

`bin/codeworld/static/models/ayakaktx2opt.glb`（神里绫华模型）与
`bin/codeworld/static/textures/` 下的全部贴图均为 **miHoYo（米哈游）版权资产**，
来源为 KallkaGo/ToonShading 仓库自带的测试资源。它们**仅限本地渲染测试使用**，
**禁止再分发、禁止商用**。模型文件不入 git（见 `bin/codeworld/static/models/README.md`）；
贴图按任务定案随仓库走私有存储（同样仅本地测试语义）。任何对外发布前必须
先移除这些资产。
