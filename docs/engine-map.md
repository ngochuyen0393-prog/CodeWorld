# Defold 引擎工程地图（engine-map.md）

> 姊妹篇：[game-menu.md](game-menu.md) 是"游戏设计系统"视角（12 章 121 条，策划官）；本篇是"引擎工程器官"视角（32 楼 ~219 个要害器官，工程官）。参照物 = 本地 Defold 1.14.0 源码（D:\Defold\defold-1.14.0\，13379 文件，Defold 宽松许可证，可读可学不可整段搬进闭源产品）。
> 用法：土豆点名"X 怎么实现的"→ 先查本表定位文件，再带行号进源码；每楼三行账管"这楼多大、多少已标、多少是借的"。
> 全部行号/文件数为 2026-10-28 实测（1.14.0 tag）。

---

## 0. 四机器分工（顶层）

一台 Defold = 四台独立机器各自编译，互相只通过文件和协议握手：

| 机器 | 语言 | 位置 | 干什么 | 和 CodeWorld 的对照 |
|---|---|---|---|---|
| **editor 编辑器** | Clojure + JavaFX | `editor/`（3024 文件，81MB） | 项目编辑、场景视图、图数据模型、事务/全局撤销 | **咱没有也不造**——玩家自带工具进场（世界只收产物不收工程）；编辑器架构档 `editor/README_SYSTEMS_OVERVIEW.md` 值得一读（图数据模型/事务/资源同步，将来做观战/调试 UI 时有参考价值） |
| **engine 引擎** | C++ | `engine/`（32 楼，全部在本文） | 跑游戏：主循环/物理/渲染/脚本 | 对应咱 `crates/* + bin`（Rust 权威进程） |
| **com.dynamo.cr = Bob** | Java | `com.dynamo.cr/`（504 文件） | 命令行内容构建器（bob.jar）：编译资源/打包/出包 | 对应咱"进场门"的资源预处理（.wat 校验/编译）；咱极简=玩家丢 .wat 即进场 |
| **external 借件库** | 各家 | `external/`（15 件，约 165MB） | skribidi/harfbuzz/sheenbidi/basisu/protobuf/opus/bullet3d/glfw/box2d/libunibreak/box2d_v2/lz4/vkquality/fir_filter/dawn | "该借全借"的同行实证——引擎自写的只有规则层，物理/字体/压缩全是业界件 |

**借件规模对比**（external 实测）：harfbuzz 48MB + skribidi 52MB（双语文字整形，比自写引擎总量还大）、basisu 13MB（纹理万能压缩）、opus 7.5MB（音频 codec）、bullet3d 5.8MB（3D 物理）、glfw 2.9MB（窗口）、box2d 2.9MB + box2d_v2 816K（两代 2D 物理）。

**engine/ 内部还藏两层借件**（不在 external/ 里）：dlib/src/mbedtls 1028 文件（TLS 加密库，占 dlib 楼 src 的 3/4）、dlib/ext/jetty 84 文件（含 javax.servlet jar，Java http 服务器，编辑器远程构建用）、script/luasocket 36 文件 + luacjson 5 + bitop 2 + lib/js 1（Lua 生态件）、profiler/remotery 10（帧分析器）。**借的比自写的多**——engine/ 32 楼合计 5370 文件（全仓 13379 另含 editor 3024/Bob 504/external 借件），刨掉 test 2915、借件（mbedtls 1028/jetty 84/lua 55/luasocket 36…）、dmsdk 公开头与出厂资源，真·自写核心约 850 文件。

---

## 1. 第一层：engine/ 32 楼层清单（六组分类）

分组沿用 CODE_STRUCTURE.md 官方库清单 + 依赖顺序（自上而下=底层先建，上层可用下层，反向禁止）。

### A. 骨架组（2 楼）——心脏和实体账本

| 楼 | 一句话职能 |
|---|---|
| **engine** | 总机：主循环 StepFrame（收输入→更游戏对象→渲染→换帧）、引擎生命周期、调试 web 服务 |
| **gameobject** | 实体系统：collection（实体集合）+ game object（空壳容器）+ component（部件）三级组装，消息分发，更新排序 |

### B. 零件库（1 楼）——所有"看得见摸得着"的游戏部件

| 楼 | 一句话职能 |
|---|---|
| **gamesys** | 零件总库：sprite/碰撞/factory/label/particlefx/model/gui 等全部部件 + 20 个 Lua 模块 + 15 种资源格式，引擎里最大的一坨自写代码 |

### C. 表现层组（9 楼）——咱剥给浏览器的那部分

| 楼 | 一句话职能 |
|---|---|
| **render** | 高层渲染：材质/渲染命令缓冲/渲染脚本（render script 也是 Lua）/视锥剔除/合批 |
| **graphics** | 图形 API 适配层：顶点缓冲/shader，下面焊 6 个平台后端（opengl/vulkan/metal/dx12/webgpu/null） |
| **font** | 文字全家桶：TTF 解析/SDF 字形/字形缓存/harfbuzz 复杂排版/双向文字，23 个 Java 文件（22 实现+1 测试）给编辑器侧用 |
| **gui** | 屏幕空间 UI 系统（2D 界面部件，gui.cpp 单文件 5153 行巨石） |
| **particle** | 粒子模拟器（被 comp_particlefx 调用的纯模拟库，4 个源文件，61 个测试全是黄金数据） |
| **sound** | 软件混音：DSP + 4 种解码器（opus/vorbis/tremolo/wav）+ 6 平台音频设备（openal/wasapi/opensl/avaudio/js/null） |
| **shaderc** | 着色器编译器：源码→SPIRV→各平台方言（spvc 转译/hlsl 直出） |
| **modelc** | 模型导入器：glTF 读入（cgltf 库）→引擎模型格式，出 .jar+.dll 给 Bob 用 |
| **texc** | 纹理编译器：预乘/翻转/抖动 + ASTC/Basis 压缩编码 |

### D. 输入组（2 楼）——手柄鼠标到"意图"

| 楼 | 一句话职能 |
|---|---|
| **hid** | 硬件输入原始层：键鼠/手柄/触摸的设备抽象（含 glfw 手柄驱动 + mac 专用驱动） |
| **input** | 输入翻译层：把 HID 原始事件按 input_binding 映射成游戏"动作"（土豆玩过的 touch 暗号翻译） |

### E. 逻辑/物理组（3 楼）——咱沙箱的亲戚

| 楼 | 一句话职能 |
|---|---|
| **script** | Lua 高层接口：脚本上下文/消息传递/vmath 数学库暴露/sys 模块/计时器，内置 luasocket 网络件 |
| **lua** | Lua 5.1 解释器原版源码（55 个 .c/.h，一行不改地 vendor） |
| **physics** | 物理前端：Box2D(2D)/Bullet(3D) 统一门面 + box2d_defold 适配层（StepWorld2D 心跳在这） |

### F. 地基/后勤组（15 楼）——不显眼但谁都靠

| 楼 | 一句话职能 |
|---|---|
| **dlib** | 标准库地基：容器/哈希/socket/线程/压缩/日志/HTTP/加密（内藏 mbedtls 1028 文件+jetty 83） |
| **ddf** | 自研数据格式（Defold Data Format）：protobuf 瘦身版，引擎所有文件格式的底座 |
| **resource** | 资源系统：工厂/引用计数/7 种加载源（file/http/zip/archive…）/异步加载队列 |
| **platform** | 窗口抽象：glfw3 五平台变体 + 各平台窗口头，把"开窗口"从引擎里隔离出去 |
| **profiler** | 性能剖析：4 平台实现 + 借来的 Remotery + 屏上 HUD 渲染 |
| **crash** | 崩溃处理：调用栈回溯（7 种 backtrace 变体×5 平台信号策略），写崩溃文件 |
| **liveupdate** | 热更新：下载内容包→验签 manifest→挂载进资源系统（咱"热更新大刀"的同行样板） |
| **record** | 视频录制：VP8 编码录屏（.avi/ivf 容器） |
| **extension** | 原生扩展注册 API：C++ 插件注册进引擎生命周期的钩子（AppInitialize/Initialize/Update…） |
| **jni** | Java/C++ 绑定工具：gen_java.py 生成 Java 侧胶水 |
| **sdk** | SDK 打包测试：生成 sdk.h 总头文件并验证全部 dmsdk 头能编过 |
| **testmain** | 单元测试主Harness：全部楼的 test_*.cpp 链进这一个 main |
| **tools** | 小工具集：编辑器启动器 launcher（.exe）/鼠标捕获 4 平台/gdc 符号导出器/纹理转换 |
| **docs** | 官方内部文档 13 篇 md + 3 个 py 文档生成器（架构金矿，本文多处引用） |
| **rig** | 骨骼动画运行时：模型+骨架姿态解算（被 model/spine 部件用；编在表现层组但按依赖属底层件） |

---

## 2. 第二层：要害器官官四栏表（核心索引）

四栏：文件 ｜ 功能一句话 ｜ 对应 CodeWorld 哪个 crate/宪法条款 ｜ 状态。
状态记号：✅=已精读定性，📌=锚点已核（行号实测），🔬=读了头部定性，📦=借件/生成物，🧪=测试资源堆，❓=未细读。

### A. 骨架组

#### engine 楼（718 文件：test 575 + content 出厂 108【builtins 103+材质 3+game.project/profiler.html 2】+ src 非测试 27【cpp/h 23+.mm/.hpp/.rc/.ico 4】+ 其他 8【darwin 2+scripts 2+proto 1+楼根 3】）

| 文件 | 功能 | 对应 CodeWorld | 状态 |
|---|---|---|---|
| `engine/src/engine.cpp` | 心脏。StepFrame:1956（一帧全套：资源重载→HID→job→声音→输入→游戏对象更新→渲染→消息分发）；dmEngine::Step:2417（固定步长累积器：dt 累积够一拍才 StepFrame，多退少补） | bin 主循环=咱拍子机；**它的固定步长累积器=拍与帧解耦的行业写法**（咱 1拍/秒+60帧插值同构，见 menu 时间章） | 📌 |
| `engine/src/engine_main.cpp` | 进程入口 engine_main()：先注册全部原生扩展符号再进循环 | bin/main.rs 启动序列 | 🔬 |
| `engine/src/engine_loop.cpp` | 外层生死循环：AppCreate→(CreateEngine→Update→Destroy)→AppDestroy；支持 reboot 不退进程 | 咱暂无对应（重启世界=重开进程）；热更新大刀要参照 | 🔬 |
| `engine/src/engine_private.h` | HEngine 全状态 struct（窗口/图形/资源/物理/声音…context 一网打尽） | World struct（crates 各 context 的聚合根） | 🔬 |
| `engine/src/engine_service.cpp` | 引擎内调试 web 服务（运行中探查/改参数） | 控制台/指挥中心的近亲；遥测仪表盘想法🔥的参照 | 🔬 |
| `engine/src/engine_service_discovery.cpp` | mDNS 局域网服务发现 | 暂无（多人时代可能要） | 🔬 |
| `engine/src/physics_debug_render.cpp` | 物理调试画线（碰撞框可视化） | render crate 调试层 | 🔬 |
| `engine/content/builtins/` | 出厂自带资源 103 件（默认材质/logo/gltf 原型/connect 集合）+ game.project 等 5 件出厂配置 | **反面教材对照**：咱宪法禁止世界自带内容（#011 同族讨论，官方示例走玩家身份进场） | 📦 |

#### gameobject 楼（349 文件：test 311 + src 非测试 28 + dmsdk 公开头 6 + proto 3 + CMakeLists 1）

| 文件 | 功能 | 对应 CodeWorld | 状态 |
|---|---|---|---|
| `gameobject/src/gameobject/gameobject.cpp` | 实体心脏。Update:3157 起（官方优先级注释 3157-3278：collectionproxy 100→script 200→anim 250→gui 300→collision 400→camera 500→model 700→mesh 725→particlefx 800→factory 900→collectionfactory 950→light 1000→sprite 1100→tilegrid 1200→label 1400）；m_FixedAccumTime:3178-3195（FixedUpdate 累积器，默认关） | 拍内两阶段"代码先物理后"的行业同构+官方排班表；**咱无组件间排序问题（实体=沙箱整体）** | 📌 |
| `gameobject/src/gameobject/gameobject.h` | 实体公开 API：Spawn/Delete/GetComponent/消息投递 | core crate 实体表 | 🔬 |
| `gameobject/src/gameobject/component.cpp` | 部件基类契约：NewWorld/Create/Update/AddToUpdate/OnMessage 钩子集 | 咱无部件概念——实体=沙箱一件套；对照价值=**Defold 的能力拼装靠部件，咱靠玩家代码** | 🔬 |
| `gameobject/src/gameobject/comp_script.cpp` | 脚本部件：把 Lua 生命周期（init/update/on_message）挂进实体更新 | 沙箱 on_tick 同族（它每帧调，咱每拍调） | 🔬 |
| `gameobject/src/gameobject/gameobject_script.cpp` | Lua 桥：go.* API 注册（go.animate/go.get/go.set） | api.md 的注册器同构 | 🔬 |
| `gameobject/src/gameobject/gameobject_props_lua.cpp` | 动态属性：go.get/set 运行时读写（含 lua↔ddf 双向） | 快照可见字段读写 | 🔬 |
| `gameobject/src/gameobject/res_collection.cpp` | collection 资源加载（.collection 文件→实体树） | 实体登记表加载 | 🔬 |
| `gameobject/proto/gameobject/` | 3 个格式定义：gameobject/lua/properties | net-protocol 的 schema 层 | 📦 |

### B. 零件库

#### gamesys 楼（1235 文件：test 991 + src 非测试 226【核心 187+java 1+dmsdk 38】+ proto 15 + 出厂材质 2 + CMakeLists 1）
| 文件 | 功能 | 对应 CodeWorld | 状态 |
|---|---|---|---|
| `gamesys/src/gamesys/gamesys.cpp:234` | **组件注册总表**（REGISTER_COMPONENT_TYPE 宏×13，官方注释明说优先级逐个 +100 均匀排班） | 组件优先级=拍内执行顺序的立法现场；咱对照=两阶段管线的行业实证 | 📌 |
| `components/comp_sprite.cpp` | 精灵部件（2809 行，全库最大部件：渲染注册/动画/九宫格） | render 层归浏览器——**咱薄客户端省掉的最大单件** | 🔬 |
| `components/comp_collision_object.cpp` | 碰撞部件：:159 起 2D/3D 分流（PHYSICS_ENGINE_BOX2D→Box2D 路径，Bullet 走 3D）——**物理引擎可替换插槽的行业实现** | physics crate；咱"世界插槽私有、接口不进玩家表"（#055/P7）的对照面：它编译期焊死，咱运行中恒定防复播 | 📌 |
| `components/comp_factory.cpp` | 工厂部件：运行时批量造实体（游戏对象模板→实例） | spawn（玩家造物出生）同族 | 🔬 |
| `components/comp_collection_factory.cpp` | 集合工厂：整棵实体树运行时实例化 | spawn 组 | 🔬 |
| `components/comp_collection_proxy.cpp` | 集合代理：子世界按需加载/卸载（优先级 100 第一个更新） | 暂无对应（分区加载时代再看） | 🔬 |
| `components/comp_camera.cpp` | 相机部件 | 镜头跟随=浏览器层实现 | 🔬 |
| `components/comp_label.cpp` | 文字部件（联动 font 楼） | 前端文字渲染归浏览器 | 🔬 |
| `components/comp_particlefx.cpp` | 粒子部件（联动 particle 楼） | 特效归玩家自绘渲染 | 🔬 |
| `components/comp_model.cpp` | 3D 模型部件（联动 rig/modelc） | 将来 3D 时代的参照 | 🔬 |
| `components/comp_gui.cpp` | UI 部件（联动 gui 楼，屏幕空间） | UI 归浏览器 | 🔬 |
| `gamesys/src/gamesys/scripts/script_physics.cpp:2145` | **Lua 物理白名单注册表**（PHYSICS_FUNCTIONS[]：raycast/create_joint/apply_force…每个 API 一行注册） | **能力接口=白名单注册表非牢房**的行业实现；咱 api.md 注册器的同构物 | 📌 |
| `scripts/`（其余 19 个 script_*.cpp） | 每 Lua 模块一文件：http/image/sprite/label/sound/tilemap/window/factory/collectionproxy… | 咱 API 面板的"每能力一个模块"组织法同构 | 🔬 |
| `resources/`（顶层 39 个 res_*.cpp，含子目录共 43） | 资源类型加载器（res_sprite/res_gui/res_font…一类型一文件，CODE_STRUCTURE 官方命名约定） | storage crate 加载器层 | 🔬 |
| `gamesys/proto/gamesys/`（15 个 ddf） | 全部内容格式：sprite/atlas/label/camera/physics/tile/gui/sound… | 快照 schema；**"每种内容一个 proto"=格式即接口** | 📦 |

### C. 表现层组

#### render 楼（43 文件：核心 30 + test 6 + proto 5 + dmsdk 1 + CMakeLists 1）

| 文件 | 功能 | 对应 CodeWorld | 状态 |
|---|---|---|---|
| `render/src/render/render.cpp` | 渲染上下文：render list 登记/材质 tag 剔除/视锥剔除/合批 | 浏览器 Canvas 之上的"列表+剔除+合批"三步=渲染管线骨架（RENDERER_FRAME.md 官方文档） | 🔬 |
| `render/src/render/render_script.cpp` | 渲染脚本：渲染流程本身也用 Lua 写（render.draw 主导一帧） | **渲染逻辑可编程**的同构——咱的玩家自绘渲染机制（浏览器侧 WASM）远亲 | 🔬 |
| `render/src/render/render_command.cpp` | 渲染命令缓冲 | 快照=下行命令流的渲染版 | 🔬 |
| `render/src/render/material.cpp` | 材质系统（shader 程序+常量+tag） | 材质库展示页（#d3d）参照 | 🔬 |
| `render/src/render/program_utils.cpp` | shader 程序管理 | — | 🔬 |
| `render/src/render/display_profiles.cpp` | 分辨率档位适配 | 暂无（浏览器自适应） | 🔬 |
| `render/src/render/debug_renderer.cpp` | 调试画线 | render crate 调试层 | 🔬 |
| `render/proto/render/`（5 个 ddf） | material/render/compute/font/render_target 格式 | — | 📦 |

#### graphics 楼（97 文件：核心 48（六后端在内）+ test 42 + dmsdk 5 + proto 1 + CMakeLists 1）

| 文件 | 功能 | 对应 CodeWorld | 状态 |
|---|---|---|---|
| `graphics/src/graphics.cpp` | 图形前端大闸：设备创建/缓冲/纹理/shader 的统一 API | 咱零负担（浏览器 WebGPU/GL 全白送） | 🔬 |
| `graphics/src/graphics_adapter.h` | 后端适配器接口（选 vulkan 还是 gl） | "接口在前实现在后"的适配层样板 | 🔬 |
| `graphics/src/opengl/`（含 win32） | OpenGL/ES 后端 | — | 📦 |
| `graphics/src/vulkan/`（android/desktop/ios/osx 四变体） | Vulkan 后端 | — | 📦 |
| `graphics/src/metal/` | macOS Metal 后端 | — | 📦 |
| `graphics/src/dx12/` | Windows DX12 后端 | — | 📦 |
| `graphics/src/webgpu/` | WebGPU 后端（浏览器/wasm 路线） | **咱 Three.js/PixiJS 底下就是它**——最亲的后端 | 📦 |
| `graphics/src/null/` | 空后端（无头跑测试用） | 咱权威进程天然"null 渲染"——**分离式架构白送它专门造一层** | 📦 |
| `graphics/src/transcoder/` | 纹理转码（basisu 运行时解码） | — | 📦 |

#### font 楼（552 文件：test 484【src/test 483+Java 侧 1】+ 核心 37【src 顶 23+render 8+truetype 2+harfbuzz 适配 2+fontc 2】+ Java 22 + dmsdk 3 + 出厂 shader 2 + scripts 1 + proto 1 + CMakeLists/README 2）

| 文件 | 功能 | 对应 CodeWorld | 状态 |
|---|---|---|---|
| `font/src/font.cpp` | 字体系统门面（字形请求→渲染） | 前端字体归浏览器 | 🔬 |
| `font/src/font_glyphbank.cpp` | 字形缓存池（用过的字形驻留） | 资源池化的教学样本 | 🔬 |
| `font/src/font_sdf.cpp` | SDF 有向距离场字形（缩放不糊） | Canvas 文字渲染可用同思路 | 🔬 |
| `font/src/font_ttf.cpp` + `font/src/truetype/` | TTF/TrueType 解析 | — | 🔬 |
| `font/src/fontc/` | 字体编译器（离线烘字形，联动 Bob） | 资产管线=定收纳口非造工具 | 🔬 |
| `font/src/harfbuzz/` | 复杂文字整形（阿拉伯/天城体连写）借 harfbuzz | — | 📦 |
| `font/src/text_layout*.cpp`（4 个变体：legacy/markup/skribidi） | 排版引擎四代同堂（skribidi=双向文字） | 文字工程深度>整个物理楼，可见"显示文字"从来不是小事 | 🔬 |
| `font/src/render/`（4 个） | 字形生成+顶点输出 | — | 🔬 |
| `font/src/java/`（22 个） | Java 侧字形栅格化（Bob/编辑器用） | — | 📦 |
| `font/proto/font/glyphbank_ddf.proto` | 字形库格式 | — | 📦 |

#### gui 楼（18 文件：核心 6 + test 10 + dmsdk 1 + CMakeLists 1）

| 文件 | 功能 | 对应 CodeWorld | 状态 |
|---|---|---|---|
| `gui/src/gui.cpp` | 屏幕空间 UI 系统（**单文件 5153 行巨石**：节点树/动画/布局/脚本沙箱） | UI 归浏览器；巨石文件=反面维护教材 | 🔬 |
| `gui/src/gui_script.cpp` | UI 脚本（gui 也有自己的 Lua 环境） | 每类实体独立脚本环境的思想近亲 | 🔬 |
| `gui/src/gui_null.cpp` | 无头 UI（测试用） | — | 📦 |

#### particle 楼（67 文件：核心 4 + test 61 + proto 1 + CMakeLists 1）

| 文件 | 功能 | 对应 CodeWorld | 状态 |
|---|---|---|---|
| `particle/src/particle.cpp` | 粒子模拟（发射器/修改器/拖尾，纯数学无渲染） | 特效=玩家自绘；**模拟与渲染分层**的干净样本 | 🔬 |
| `particle/src/particle_null.cpp` | 空实现 | — | 📦 |
| `particle/proto/particle/particle_ddf.proto` | 粒子格式 | — | 📦 |
| `particle/src/test/`（61 个） | 黄金数据测试（粒子轨迹采样比对） | 确定性测试锚的同行写法 | 🧪 |

#### sound 楼（72 文件：核心 31 + test 38 + dmsdk 1 + CMakeLists 1 + README 1）

| 文件 | 功能 | 对应 CodeWorld | 状态 |
|---|---|---|---|
| `sound/src/sound.cpp` | 软件混音器（音源登记/音量组/暂停） | 音频插座⬜未立（玩家之旅五站最后一站） | 🔬 |
| `sound/src/sound_dsp.h` + `dsimd.h` | DSP 处理 + SIMD 加速宏 | — | 🔬 |
| `sound/src/decoders/`（4 个） | 解码器四格式：opus/vorbis(stb)/tremolo/wav | — | 📦 |
| `sound/src/devices/`（6 个） | 音频输出设备六平台：openal/wasapi/opensl/avaudio(js 之外各平台原生)/js/null | 设备适配层的多平台胶水样本 | 📦 |
| `sound/src/stb_vorbis/` | vorbis 解码 vendored | — | 📦 |

#### shaderc 楼（27 文件：shaderc 核心 7 + Java 桥 2 + jni 桥 2 + standalone 独立版 2 + test 13 + CMakeLists 1）

| 文件 | 功能 | 对应 CodeWorld | 状态 |
|---|---|---|---|
| `shaderc/src/shaderc.cpp` | 着色器编译总控（解析/预处理） | 材质库时代参照 | 🔬 |
| `shaderc/src/shaderc_spirv.cpp` | 产 SPIRV 中间码 | — | 🔬 |
| `shaderc/src/shaderc_spvc.cpp` | SPIRV-Cross 转译各平台方言 | **一次编写多方言**=管线资产思想 | 🔬 |
| `shaderc/src/shaderc_hlsl.cpp` | HLSL 直出路径 | — | 🔬 |
| `shaderc/src/standalone/` | 独立命令行版（不进引擎） | — | 📦 |

#### modelc 楼（33 文件：核心 10 + test 20 + scripts 1 + CMakeLists 1 + README 1）

| 文件 | 功能 | 对应 CodeWorld | 状态 |
|---|---|---|---|
| `modelc/src/modelimporter.cpp` | 模型导入总控 | **glTF 插座（#052）的同行实现**：咱收 glTF 它也收 glTF | 🔬 |
| `modelc/src/modelimporter_gltf.cpp` | cgltf 库读 glTF | 同上 | 🔬 |
| `modelc/src/modelimporter_debug.cpp` | 调试导出（人读中间格式） | — | 🔬 |
| `modelc/src/jni/` + `modelc/src/java/` | Java 桥（给 Bob 调） | — | 📦 |

#### texc 楼（19 文件：核心 12 + test 5 + scripts 1 + CMakeLists 1）

| 文件 | 功能 | 对应 CodeWorld | 状态 |
|---|---|---|---|
| `texc/src/texc.cpp` | 纹理编译门面：预乘 alpha/翻转/抖动（:107-137 三个处理函数注释清晰） | 资产预处理参照 | 🔬 |
| `texc/src/texc_enc_astc.cpp` | ASTC 压缩编码 | — | 🔬 |
| `texc/src/texc_enc_basis.cpp` | Basis Universal（一次压缩全平台） | — | 📦 |
| `texc/src/texc_buffer.cpp` | 像素缓冲操作 | — | 🔬 |

#### rig 楼（9 文件：核心 4 + test 2 + dmsdk 1 + proto 1 + CMakeLists 1）

| 文件 | 功能 | 对应 CodeWorld | 状态 |
|---|---|---|---|
| `rig/src/rig.cpp` | 骨骼姿态解算（骨骼矩阵缓存 NULL_ANIMATION 哈希锚） | 骨骼动画包（D-017）的运行时侧参照；**角色用骨骼不用物理关节**（1025 定论）的官方件 | 🔬 |
| `rig/src/rig_null.cpp` | 空实现 | — | 📦 |
| `rig/proto/rig/rig_ddf.proto` | 骨架格式 | — | 📦 |

### D. 输入组

#### hid 楼（17 文件：核心 11（含 darwin .mm 与 glfw/sdl 映射头）+ test 3【src 2+根 test_app_hid.sh 1】+ dmsdk 2 + CMakeLists 1）

| 文件 | 功能 | 对应 CodeWorld | 状态 |
|---|---|---|---|
| `hid/src/hid.cpp` | 输入设备抽象门面（键盘/鼠标/触摸/手柄四源） | 浏览器事件=白送的 hid 层 | 🔬 |
| `hid/src/native/hid_gamepad_driver_glfw.cpp` | 手柄驱动（glfw 后端） | — | 📦 |
| `hid/src/native/hid_native.cpp` | 原生输入路径 | — | 🔬 |
| `hid/src/darwin/gamepad_darwin.mm` | mac 专用手柄驱动 | 平台胶水样本 | 📦 |
| `hid/src/hid_guid.cpp` | 设备 GUID 映射 | — | 🔬 |

#### input 楼（12 文件：核心 3 + test 7 + proto 1 + CMakeLists 1）

| 文件 | 功能 | 对应 CodeWorld | 状态 |
|---|---|---|---|
| `input/src/input.cpp` | 意图翻译层：HID 事件+input_binding→action（土豆玩过的 touch 暗号翻译的引擎侧） | 意图通道（点击三分流）的行业版；**咱极简=点击一种，它全量** | 🔬 |
| `input/proto/input/input_ddf.proto` | binding 格式 | — | 📦 |

### E. 逻辑/物理组（咱沙箱的亲戚）

#### script 楼（143 文件：test 58【src/test 57+楼根 py 1】+ 核心 38【src 34+ddf 4】+ 借件 44【luasocket 36+luacjson 5+bitop 2+lib/js 1】+ dmsdk 1 + CMakeLists/README 2）

| 文件 | 功能 | 对应 CodeWorld | 状态 |
|---|---|---|---|
| `script/src/script.cpp` | Lua 上下文管理：创建/重置/模块加载 | arena 沙箱管理同构（它重置 Lua 状态=咱每拍唤醒） | 🔬 |
| `script/src/script_msg.cpp` | 消息传递（post_message 的 Lua 侧） | 外桥 bridge_send/recv 近亲（一收一发寄信模型） | 🔬 |
| `script/src/script_vmath.cpp` | vmath 数学库暴露给 Lua | api.md 数值接口 | 🔬 |
| `script/src/script_sys.cpp` | sys 模块（系统信息/重启/保存） | — | 🔬 |
| `script/src/script_timer.cpp` | 计时器（拍数换算的样本） | 每拍计时器 | 🔬 |
| `script/src/script_html5.cpp` | 浏览器特化（js 桥） | 浏览器层交互参照 | 🔬 |
| `script/src/luasocket/`（36 文件） | Lua 网络库 vendored（tcp/udp/select） | — | 📦 |
| `script/src/luacjson/` + `script/src/bitop/` | JSON 解析+位运算 vendored | — | 📦 |
| `script/src/script_zlib.cpp` | 压缩暴露 | — | 🔬 |

#### lua 楼（83 文件：Lua 原版 55 + doc_h 9 + lua_asan 1 + dmsdk 4 + cs 绑定 5 + test 5 + scripts 2 + CMakeLists/sdk_gen 2）

| 文件 | 功能 | 对应 CodeWorld | 状态 |
|---|---|---|---|
| `lua/src/lua/`（55 个 .c/.h） | **Lua 5.1 解释器原版**（lapi/lvm/lgc/lparser…一行不改 vendor 进来） | wasmtime 的对应物：**跑玩家代码的机器**。Lua=30 年沙箱脚本活样本，wasmtime=系统级沙箱——同一家族两代产品 | 📦 |
| `lua/src/dmsdk/lua/`（4 头文件） | 对外公开的 Lua API 头 | — | 📦 |
| `lua/src/*.doc_h`（9 个） | 各标准库的文档注释（生成官方手册用） | **手册即代码**（doc_h 与源码同库）——api.md 生成思路参照 | 📦 |
| `lua/src/lua_asan.cpp` | AddressSanitizer 内存消毒挂钩 | 测试基建 | 🔬 |

#### physics 楼（24 文件：源码 18【box2d 适配 4+box2d_defold 适配 4+3D 适配 5+前端/公共 5】+ test 4 + dmsdk 1 + CMakeLists 1）

| 文件 | 功能 | 对应 CodeWorld | 状态 |
|---|---|---|---|
| `physics/src/physics/physics.cpp` | 2D/3D 统一前端：上下文创建/Step 分发 | physics crate 门面同构；**咱单物理 vs 它双物理按需选** | 🔬 |
| `physics/src/physics/box2d_defold/box2d_defold_physics.cpp` | **2D 物理心跳 StepWorld2D:439**（三步：UpdateKinematic 游戏侧→物理侧同步→World.Step 联立解算→写回——**双账本每帧对账两次**） | 咱单一真相源=架构更干净（已定论勿翻案）；此处是对照弹药 | 📌 |
| `physics/src/physics/box2d_defold/box2d_defold_debug_draw.cpp` | 物理调试画线 | — | 🔬 |
| `physics/src/physics/box2d/` + `box2d_defold/` | **两套 Box2D 适配层**（各 4 文件；Box2D 本体在 external/——box2d 2.9MB+box2d_v2 816K，楼内只是 include 桥，非 vendored 源码） | Box2D 60Hz 功力不适配拍制世界（调研定论勿再查）；内嵌稳定老物理不追新的活例 | 📦 |
| `physics/src/physics/physics_3d.cpp` | Bullet 3D 适配（Bullet 本体在 external/） | 3D 时代参照 | 🔬 |
| `physics/src/physics/physics_common.cpp` | 2D/3D 共用（接触回调矩阵等） | — | 🔬 |

### F. 地基/后勤组

#### dlib 楼（1518 文件：mbedtls 借件 1028 + test 116 + 自写核心 167【dlib/ 170 去 testutil 3】+ testutil 3 + dmsdk 48 + zip/stb/cs 等借件胶水 14 + doc 图片 41 + jetty 84【含 servlet jar】+ scripts 10 + bin/include/proto/楼根 7）

| 文件 | 功能 | 对应 CodeWorld | 状态 |
|---|---|---|---|
| `dlib/src/dlib/vmath.h` | 向量数学 typedef（:45 起，真身=索尼 Vectormath 库借用） | 咱元组+f64 直写；**数学库=纯函数零迁移的债**（1025 定论：需要那天装 glam 即完事） | 📌 |
| `dlib/src/dlib/math.h` | **只有 120 行**：贝塞尔+Rand(seed) 种子随机（:88 Rand/:97 Rand01——**确定性回放的行业写法**，世界加随机必带种子禁真随机） | 确定性红线 #044 的旁证；卡面记的"175 行"= vmath.h 55+math.h 120 合计 | 📌 |
| `dlib/src/dlib/hash.cpp` | dmhash：字符串→64 位 id（全引擎的名字系统） | EntityId 定序（多目标取 t 最小者）用的同名思路 | 🔬 |
| `dlib/src/dlib/message.cpp` | 消息队列（跨系统投递的底座） | 外桥信箱 | 🔬 |
| `dlib/src/dlib/configfile.cpp` | 配置文件（game.project 解析） | config（CODEWORLD_MAX_SPEED 等常数的家） | 🔬 |
| `dlib/src/dlib/jobsystem.cpp` | job 系统（工作窃取线程池） | 沙箱军团调度（拍速升级时代参照） | 🔬 |
| `dlib/src/dlib/socket*.cpp`（6 平台变体） | socket 抽象：posix/win32/linux/android/web 各一份 | 咱=浏览器 WebSocket 白送 | 📦 |
| `dlib/src/dlib/http_*.cpp + http.cpp（client/server/service/cache，含 _null 变体共 12 个 http 文件） | HTTP 全家（客户端/服务器/缓存） | 起服探针/遥测参照 | 🔬 |
| `dlib/src/dlib/log.cpp` + `log_win32.cpp` | 日志系统（跨平台） | tracing（咱已有） | 🔬 |
| `dlib/src/dlib/trig_lookup.cpp` + `scripts/trig_lookup_template.h` | **生成代码的代码**：gen_trig_lookup.py 生成三角函数速查表 | 工业手法：机械数据脚本生成勿手写 | ✅ |
| `dlib/src/dlib/memprofile.cpp` + `dlib/bin/memprofile.sh` | 内存剖析（启动内存快照） | — | 🔬 |
| `dlib/src/mbedtls/`（1028 文件） | TLS 加密库 vendored（https 通道的底座） | 远程推送时代的件，现在不需要 | 📦 |

#### ddf 楼（37 文件：核心 19【C++ 16+自举 proto 3】+ ddfc 编译器 5【py+cxx/bat+java/bat】+ test 8 + dmsdk 1 + doc 1 + 楼根 3【CMakeLists/TODO/.pydevproject】）

| 文件 | 功能 | 对应 CodeWorld | 状态 |
|---|---|---|---|
| `ddf/src/ddf/ddf.cpp` | DDF 运行时：类型描述符注册/序列化（protobuf 瘦身版） | 快照信封（serde_json）；**"自研格式而非裸用 protobuf"**=引擎作者对数据格式的中间态选择 | 🔬 |
| `ddf/src/ddf/ddf_save.cpp` / `ddf_load.cpp` | 存/取 | — | 🔬 |
| `ddf/src/ddf/ddf_message.cpp` | DDF 消息封装 | — | 🔬 |
| `ddf/src/ddf/*.proto`（3 个：math/struct/extensions） | ddf 自己的元格式（math/struct/extensions） | — | 📦 |

#### resource 楼（89 文件：核心 46 + test 36 + dmsdk 3 + proto 2 + CMakeLists/sdk_gen 2）

| 文件 | 功能 | 对应 CodeWorld | 状态 |
|---|---|---|---|
| `resource/src/resource.cpp` | 资源工厂：创建/引用计数/重载（:80-86 官方 TODO 自嘲两个 hash 表） | storage crate；**引用计数=内存式资源生死账**（咱实体登记表同族） | 🔬 |
| `resource/src/providers/`（7 个） | 加载源七路：file/http/zip/archive/provider 总线… | .wat 进场=最简 provider；远程推送时代再扩 | 🔬 |
| `resource/src/mount/`（3 个） | 挂载点：android/generic/mmap | — | 📦 |
| `resource/src/async/load_queue*.cpp`（3 模式） | 异步加载队列（同步/线程/双模） | 快照断线补拉参照 | 🔬 |
| `resource/src/resource_manifest.cpp` | 资源清单（hash 校验） | 热更新验签参照 | 🔬 |
| `resource/src/resource_preloader.cpp` | 预加载器（进世界先拉全） | 开局加载 | 🔬 |
| `resource/src/resource_archive.cpp` | 打包档案（.darc 格式，docs/ARCHIVE_FORMAT.md 有格式文档） | — | 🔬 |
| `resource/proto/resource/liveupdate_ddf.proto` | 热更新清单格式 | — | 📦 |

#### platform 楼（28 文件：核心 21（glfw 五平台变体在内）+ test 5 + dmsdk 1 + CMakeLists 1）

| 文件 | 功能 | 对应 CodeWorld | 状态 |
|---|---|---|---|
| `platform/src/window.cpp` | 窗口抽象门面（开窗/轮询事件） | 咱零负担（浏览器是窗口） | 🔬 |
| `platform/src/platform_window_glfw3.cpp` + `_win32/_linux/_vulkan` 变体 | glfw3 五平台变体 | 平台胶水样本 | 📦 |
| `platform/src/platform_window_*.h`（7 个平台头） | android/ios/linux/osx/win32/opengl/vulkan 各自的窗口接口 | — | 📦 |
| `platform/src/platform_window_null.cpp` | 无头窗口（CI 测试用） | 咱天然无头 | 📦 |

#### profiler 楼（31 文件：核心 17 + remotery 借件 10 + test 3 + CMakeLists 1）

| 文件 | 功能 | 对应 CodeWorld | 状态 |
|---|---|---|---|
| `profiler/src/profiler.cpp` | 剖析 API 门面（帧时间/计数器/内存三表） | 遥测仪表盘想法🔥的直接参照 | 🔬 |
| `profiler/src/profiler_win32.cpp`（+linux/android/web 4 变体） | 平台计时实现 | — | 📦 |
| `profiler/src/remotery/`（10 文件） | Remotery 帧分析器 vendored（浏览器看帧） | 控制台的"看"面参照 | 📦 |
| `profiler/src/profiler_render.cpp` | 屏上 HUD（画帧率/内存条） | 指挥中心渲染参照 | 🔬 |
| `profiler/src/profiler_proc_utils.cpp` | 进程数据采集（CPU/内存占用） | — | 🔬 |

#### crash 楼（35 文件：核心 27（其中十九个平台变体）+ test 7 + CMakeLists 1）

| 文件 | 功能 | 对应 CodeWorld | 状态 |
|---|---|---|---|
| `crash/src/crash.cpp` | 崩溃处理门面：信号钩子→调用栈→写文件 | Rust panic+backtrace 天然有；**"崩溃必须留案底"**的工程自觉 | 🔬 |
| `crash/src/backtrace_*.cpp`（7 变体：execinfo/libunwind×2/null/web/win32/android，glob 另含 4 个信号策略文件） | 调用栈回溯七路：execinfo/libunwind/ndk/null/web/win32… | 平台胶水之最 | 📦 |
| `crash/src/backtrace_signal_policy_*.cpp`（darwin/linux/default 3 平台+posix 公共层） | 信号策略（哪些信号算崩溃）：darwin/linux/default 三份+posix 公共头 | — | 📦 |
| `crash/src/load_addrs_*.cpp`（4 平台） | 模块加载地址（符号还原用） | — | 📦 |
| `crash/src/script_crash.cpp` | Lua 侧崩溃信息暴露 | — | 🔬 |

#### liveupdate 楼（13 文件：核心 5 + test 7 + CMakeLists 1）

| 文件 | 功能 | 对应 CodeWorld | 状态 |
|---|---|---|---|
| `liveupdate/src/liveupdate.cpp` | 热更新主逻辑：manifest 验签→挂载（:25 起 Result 映射表） | **热更新大刀（战场实时改代码）的行业标准样板**：验签+挂载+不重启 | 🔬 |
| `liveupdate/src/script_liveupdate.cpp` | Lua API（lua_ref 打包） | — | 🔬 |
| `engine/docs/LIVEUPDATE.md` | 官方热更新文档 | 立法前必读 | ✅ |

#### record 楼（9 文件：核心 4 + test 3 + CMakeLists/.gitignore 2）

| 文件 | 功能 | 对应 CodeWorld | 状态 |
|---|---|---|---|
| `record/src/record/record.cpp` | 视频录制：VP8 编码（:25 include vpx/vp8cx.h，:55 vpx_codec_ctx） | 将来"战报回放"可参照（确定性回放的录制端咱已有：快照即录像） | 🔬 |
| `record/src/record/record_common.cpp` | 公共（参数校验） | — | 🔬 |
| `record/src/record/record_null.cpp` | 空实现 | — | 📦 |

#### extension 楼（19 文件：核心 8 + dmsdk 3 + test 6 + CMakeLists/sdk_gen 2）

| 文件 | 功能 | 对应 CodeWorld | 状态 |
|---|---|---|---|
| `extension/src/extension.cpp` | 原生扩展注册 API：AppInitialize/Initialize/Update 生命周期钩子 | **世界插槽（#055）的对照**：它=编译期 C++ 插件注册；咱=crates 模块+接口不进玩家表 | 🔬 |
| `extension/src/extension_android.cpp` / `_ios.cpp` | 移动端 JNI/生命周期适配 | — | 📦 |
| `extension/src/dmsdk/extension/extension.hpp` | 注册宏（DM_EXTENSION_…） | — | 📦 |

#### jni 楼（14 文件：核心 2 + scripts 生成器 5 + test 5 + CMakeLists/.gitignore 2）

| 文件 | 功能 | 对应 CodeWorld | 状态 |
|---|---|---|---|
| `jni/src/jni/jni_util.cpp` | Java↔C++ 绑定工具 | 咱无 Java 层 | 🔬 |
| `jni/scripts/gen_java.py` | **生成 Java 绑定的代码**（自动化胶水） | 代码生成手法第二例 | 🔬 |

#### sdk 楼（9 文件：核心 2【sdk.h+dmsdk.csproj】+ test 5 + CMakeLists/README 2）

| 文件 | 功能 | 对应 CodeWorld | 状态 |
|---|---|---|---|
| `sdk/src/dmsdk/sdk.h` | 生成的 dmsdk 总头（把全部公开头串起来验证能编过） | **公开 API 面=要验证的合同**；咱 api.md 的编译级对应物 | 🔬 |

#### testmain 楼（6 文件：Harness 3【testmain/app/testmain.h】+ src/test 2【自测试+CMakeLists】+ 楼根 CMakeLists 1）

| 文件 | 功能 | 对应 CodeWorld | 状态 |
|---|---|---|---|
| `testmain/src/common/testmain.cpp` | 单测总 Harness：全楼 test_*.cpp 链这一个 main | cargo test 白送——**Rust 工具链优势的具象** | 🔬 |

#### tools 楼（19 文件：核心 14（launcher 的 .rc/.ico 在内）+ scripts 1 + test 3 + CMakeLists 1）

| 文件 | 功能 | 对应 CodeWorld | 状态 |
|---|---|---|---|
| `tools/src/launcher/`（10 文件含 .rc/.ico） | 编辑器启动器（小 .exe 拉起 JVM） | 双击 start.bat 的近亲 | 🔬 |
| `tools/src/mouse_capture/`（4 平台） | 鼠标捕获工具 | — | 📦 |
| `tools/src/gdc/main.cpp` | 符号导出器（dmExportedSymbols） | — | 🔬 |
| `tools/src/texconvert/main.cpp` | 纹理转换 CLI | — | 📦 |

#### docs 楼（25 文件：md 文档 13 + py 5【生成器 3+测试 2】+ lua 注解 yaml 1 + testdata 4 + CMakeLists/.gitignore 2）

| 文件 | 功能 | 对应 CodeWorld | 状态 |
|---|---|---|---|
| `engine/docs/CODE_STRUCTURE.md` | **官方库清单+依赖顺序**（ENGINE_LIBS 顺序=允许谁用谁，反向禁止） | 本文档第一层的权威出处；咱 crates 依赖图的官方同款 | ✅ |
| `engine/docs/ENGINE_LIFECYCLE.md` | 官方引擎生命周期（AppCreate→Init→Step 主循环伪代码→Destroy 逆序） | 拍子机的引擎版全图 | ✅ |
| `engine/docs/DESIGN_PHILOSOPHY.md` | 设计哲学：**"最快的代码是不写的代码"**/加功能先设计评审/向后兼容/增量交付 | 咱"只加解决当前痛点功能"的官方同款（土豆务实工程原则的 17 年同行版） | ✅ |
| `engine/docs/RENDERER_FRAME.md` | 渲染帧官方文档（render script→render list→剔除→合批→RenderObject→draw call） | 浏览器渲染层的管线知识 | ✅ |
| `engine/docs/ARCHIVE_FORMAT.md` | 打包档案格式 | — | 🔬 |
| `engine/docs/FONTS_AND_TEXT.md` | 字体系统文档 | — | 🔬 |
| `engine/docs/INPUT_SYSTEM.md` | 输入系统文档 | — | 🔬 |
| `engine/docs/NATIVE_EXTENSIONS.md` | 原生扩展文档 | — | 🔬 |
| `engine/docs/DEBUG_PORTS_AND_SERVICES.md` | 调试端口清单 | — | 🔬 |

---

## 3. 每楼三行账（实测对账表）

格式：文件总数 ｜ 已标数（核心中已进第二层表的数目）｜ 全量分账。
口径（find 可复现）：test=路径含 test 的文件；proto=楼/proto 目录；dmsdk=src/dmsdk 公开头；借件=vendored 第三方；核心=src 非测试非 dmsdk 的源码；CMakeLists/README/sdk_gen/.gitignore 等楼根构建件单列。**每行分项相加=总数**（2026-10-28 逐楼 find 实测闭合，R1 审查后全量重跑）。

| 楼 | 总数 | 已标 | 归堆账（非核心文件去向） |
|---|---|---|---|
| engine | 718 | 8 | test 575 + content 出厂 108【builtins 103+材质 3+配置 2】+ src 核心 27 + 其他 8【darwin 2+scripts 2+proto 1+楼根 3】 |
| gameobject | 349 | 8 | test 311 + src 核心 28 + dmsdk 6 + proto 3 + CMakeLists 1 |
| gamesys | 1235 | 15 | test 991 + src 非测试 226【核心 187+java 1+dmsdk 38】+ proto 15 + 出厂材质 2 + CMakeLists 1 |
| dlib | 1518 | 12 | mbedtls 1028 + test 116 + 自写核心 167 + testutil 3 + dmsdk 48 + zip/stb/cs 等借件胶水 14 + doc 图片 41 + jetty 84【含 servlet jar】+ scripts 10 + bin/include/proto/楼根 7 |
| script | 143 | 9 | test 58 + 核心 38【src 34+ddf 4】+ 借件 44【luasocket 36+luacjson 5+bitop 2+lib/js 1】+ dmsdk 1 + CMakeLists/README 2 |
| lua | 83 | 4 | Lua 原版 55 + doc_h 9 + lua_asan 1 + dmsdk 4 + cs 5 + test 5 + scripts 2 + CMakeLists/sdk_gen 2 |
| physics | 24 | 6 | 源码 18【两套适配 8——本体在 external/ 非楼内】+ test 4 + dmsdk 1 + CMakeLists 1 |
| render | 43 | 8 | 核心 30 + test 6 + proto 5 + dmsdk 1 + CMakeLists 1 |
| graphics | 97 | 9 | 核心 48（六后端在内）+ test 42 + dmsdk 5 + proto 1 + CMakeLists 1 |
| font | 552 | 10 | test 484 + 核心 37 + Java 22 + dmsdk 3 + 出厂 shader 2 + scripts 1 + proto 1 + CMakeLists/README 2 |
| gui | 18 | 3 | 核心 6 + test 10 + dmsdk 1 + CMakeLists 1 |
| particle | 67 | 4 | 核心 4 + test 61（黄金轨迹数据）+ proto 1 + CMakeLists 1 |
| sound | 72 | 5 | 核心 31（stb_vorbis 在内）+ test 38 + dmsdk 1 + CMakeLists/README 2 |
| shaderc | 27 | 5 | shaderc 核心 7 + Java 桥 2 + jni 桥 2 + standalone 2 + test 13 + CMakeLists 1 |
| modelc | 33 | 4 | 核心 10（java/jni 桥在内）+ test 20 + scripts 1 + CMakeLists/README 2 |
| texc | 19 | 4 | 核心 12 + test 5 + scripts 1 + CMakeLists 1 |
| rig | 9 | 3 | 核心 4 + test 2 + dmsdk 1 + proto 1 + CMakeLists 1 |
| hid | 17 | 5 | 核心 11 + test 3【src 2+根 .sh 1】+ dmsdk 2 + CMakeLists 1 |
| input | 12 | 2 | 核心 3 + test 7 + proto 1 + CMakeLists 1 |
| resource | 89 | 8 | 核心 46 + test 36 + dmsdk 3 + proto 2 + CMakeLists/sdk_gen 2 |
| ddf | 37 | 4 | 核心 19【C++ 16+proto 3】+ ddfc 编译器 5 + test 8 + dmsdk 1 + doc 1 + 楼根 3 |
| platform | 28 | 4 | 核心 21（glfw 五平台在内）+ test 5 + dmsdk 1 + CMakeLists 1 |
| profiler | 31 | 5 | 核心 17【src 顶 14+basic 1+js 2】+ remotery 借件 10 + test 3 + CMakeLists 1 |
| crash | 35 | 5 | 核心 27（其中 backtrace 七变体+信号策略+地址采集共十九个平台变体）+ test 7 + CMakeLists 1 |
| liveupdate | 13 | 3 | 核心 5 + test 7 + CMakeLists 1 |
| record | 9 | 3 | 核心 4 + test 3 + CMakeLists/.gitignore 2 |
| extension | 19 | 4 | 核心 8（cs 胶水在内）+ dmsdk 3 + test 6 + CMakeLists/sdk_gen 2 |
| jni | 14 | 2 | 核心 2 + scripts 生成器 5（gen_java.py 等）+ test 5 + CMakeLists/.gitignore 2 |
| sdk | 9 | 2 | 核心 2【sdk.h+dmsdk.csproj】+ test 5 + CMakeLists/README 2 |
| testmain | 6 | 1 | Harness 3 + src/test 2（自测试+CMakeLists）+ 楼根 CMakeLists 1 |
| tools | 19 | 5 | 核心 14（.rc/.ico 在内）+ scripts 1 + test 3 + CMakeLists 1 |
| docs | 25 | 9 | md 13 + py 5【生成器 3+测试 2】+ lua 注解 yaml 1 + testdata 4 + CMakeLists/.gitignore 2 |

**三处规律**（土豆问"这楼怎么这么多文件"时的标准答案）：
1. **测试占半壁**：engine 575/718、gamesys 991/1235、font 484/552——文件数唬人，真核心极小（engine 楼 src 非测试只有 27 文件，cpp/h 23）。
2. **借件藏在楼里**：dlib 名义 1518 文件，真自写核心 167（dlib/ 目录 170 去 testutil 3）——"标准库地基层 2/3 是别人的"。
3. **平台胶水成灾**：crash 楼 27 个核心文件里 backtrace×7+信号策略×5+地址×4+文件×3=19 个是平台变体——跨平台真实成本（咱单浏览器平台不背）。

---

## 4. "未定性"名单（诚实清单，宁多勿假）

以下文件**只看了名字/路径，没读内容**，本文档对其描述仅到目录级：

- `dlib/src/mbedtls/` 1028 文件——TLS 库内部结构未看（也不需要看）
- `gamesys/src/gamesys/test/` 991 文件——各部件测试的覆盖面未盘点
- `engine/src/test/` 575 文件——同上
- `font/src/java/` 23 个（22 实现+1 测试）——Java 侧字形栅格化的具体分工未读
- `graphics/src/` 六后端（opengl/vulkan/metal/dx12/webgpu/null）——只确认存在与文件数，后端内部实现一行没读
- `physics/src/physics/box2d/`——Box2D v2/v3 vendored 源码未读（60Hz 不适配已定论，无需深读）
- `editor/` 3024 文件——只读了 README_SYSTEMS_OVERVIEW.md 一篇（编辑器单独成节时再深挖）
- `com.dynamo.cr/` 504 文件——Bob 构建器内部未读
- `script/src/luasocket/` 42 文件——vendored 未读
- 各楼 `res_*.cpp` 的具体格式字段——只确认"一类型一文件"，字段未对
- `engine/docs/` 中 ARCHIVE_FORMAT/ATLAS_FORMAT/CODE_STYLE/DEBUG_PORTS_AND_SERVICES/FONTS_AND_TEXT/INPUT_SYSTEM/NATIVE_EXTENSIONS/README 八篇——确认存在与标题，未全文读

---

## 5. 附：读码地图锚点速查（已核行号）

| 锚点 | 位置 | 一句话 |
|---|---|---|
| 主循环心脏 | engine/engine/src/engine.cpp:1956 | StepFrame：一帧全套（输入→更新→渲染） |
| 步长分发 | engine/engine/src/engine.cpp:2417 | dmEngine::Step：固定步长累积器 |
| 组件排班表 | engine/gamesys/src/gamesys/gamesys.cpp:234 | REGISTER_COMPONENT_TYPE×13，优先级+100 均布 |
| 实体更新+优先级注释 | engine/gameobject/src/gameobject/gameobject.cpp:3157-3278 | Update(Collection)+官方顺序注释 |
| FixedUpdate 累积器 | engine/gameobject/src/gameobject/gameobject.cpp:3178-3195 | m_FixedAccumTime（默认关） |
| 物理心跳 | engine/physics/src/physics/box2d_defold/box2d_defold_physics.cpp:439 | StepWorld2D 三步（同步→Step→写回） |
| 2D/3D 分流 | engine/gamesys/src/gamesys/components/comp_collision_object.cpp:159 | PHYSICS_ENGINE_BOX2D 分支 |
| Lua 物理白名单 | engine/gamesys/src/gamesys/scripts/script_physics.cpp:2145 | PHYSICS_FUNCTIONS[] 注册表 |
| vmath 借用证据 | engine/dlib/src/dmsdk/dlib/vmath.h:45 | typedef Vectormath::Aos::Point3 |
| 种子随机 | engine/dlib/src/dlib/math.h:88 | Rand(uint32_t* seed) 确定性设计 |
| 生成代码 | engine/dlib/scripts/（gen_trig_lookup.py 等 8 个工具脚本）+ trig_lookup.cpp | gen_trig_lookup.py 三角速查表 |

（行号基于 1.14.0 tag，2026-10-28 实测；升级源码版本后需复核。）
