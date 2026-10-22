# models/ —— 测试模型（不入 git）

`ayakaktx2opt.glb`（8.5MB，神里绫华）是 miHoYo 版权测试资产，**不入 git**
（二进制大文件 + 版权资产不再分发的定案）。

## 从上游 workspace 拷贝（获取方式）

```bash
UP="C:/Users/Administrator/AppData/Local/hermes/kanban/boards/codeworld/workspaces/t_e8d28006/ToonShading"
cp "$UP/static/models/ayakaktx2opt.glb" ./ayakaktx2opt.glb
```

（或者从 KallkaGo/ToonShading 仓库 static/models/ 取同名文件。）

## 本卡实测数据

- 8,831,732 字节；**无** KHR_draco_mesh_compression 扩展（二进制检测，D-004b），
  加载链不接 DRACOLoader
- **有** KHR_texture_basisu（KTX2 纹理）：加载链必须接 KTX2Loader +
  basis transcoder（./vendor/basis/）
- 材质命名：face / hair / dress / body（GenshinToonMaterial.applyToSkeleton
  按名分派贴图组）
