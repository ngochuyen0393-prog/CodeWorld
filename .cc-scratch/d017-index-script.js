(function () {
  'use strict';

  // ---- 插值与连接参数 ----
  var DELAY_FACTOR = 1.5;              // 渲染延迟 = 1.5 × 平均帧间隔（渲染在过去）
  var EMA_ALPHA = 0.2;                 // 帧间隔滚动平均系数（小=平滑，大=跟随快）
  var MAX_FRAMES = 12;                 // 快照缓冲上限（≥2 即可插值，留余量抗抖动）
  var INTERVAL_MIN = 16;               // 帧间隔合理域下限（防异常小值）
  var INTERVAL_MAX = 10000;            // 帧间隔合理域上限（防异常大值污染平均）
  var BACKOFF_INIT = 1000;             // 重连退避起步（ms）
  var BACKOFF_MAX = 30000;             // 重连退避封顶（ms）
  var CAM_LERP = 0.12;                 // 镜头跟随平滑系数（每帧，60fps；0.08-0.15 推荐域）
  var CAM_ZOOM = 2;                    // 跟随窗口 = viewport 的 1/Z（跟随模式 P 周边放大观感）
  var PANEL_ACK_MS = 120;              // 登出卡片按下回执时长（视觉回执一拍再发消息）
  var PANEL_FADE_MS = 250;             // 登出弹窗淡出时长（0.25s，确认弹窗关门标准）

  // ---- DOM ----
  var stage = document.getElementById('stage');
  var cv = document.getElementById('cv');
  var ctx = cv.getContext('2d');
  var captionEl = document.getElementById('caption');
  var connEl = document.getElementById('conn');
  var tickEl = document.getElementById('tick');
  var cntEl = document.getElementById('cnt');
  var assetsEl = document.getElementById('assets');
  var itvEl = document.getElementById('itv');
  var mouseEl = document.getElementById('mouse');
  var tipEl = document.getElementById('tip');
  var camBtnEl = document.getElementById('camBtn');
  var logoutBtnEl = document.getElementById('logoutBtn');
  var logoutPanelEl = document.getElementById('logoutPanel');
  var logoutCancelEl = document.getElementById('logoutCancel');
  var logoutBannerEl = document.getElementById('logoutBanner');
  var bannerTextEl = document.getElementById('bannerText');
  var reenterBtnEl = document.getElementById('reenterBtn');
  var dcBannerEl = document.getElementById('dcBanner');
  var dcBannerTextEl = document.getElementById('dcBannerText');

  // ---- 状态 ----
  var buf = [];                 // 快照缓冲 [{t:接收时刻, tick, caption, viewport, ents:Map(id->e)}]
  var avgInterval = null;       // 帧到达间隔滚动平均（自适应 tick 周期）
  var lastRecvT = null;
  var vp = null;                // 当前视口域（来自快照，世界坐标 <-> 画布换算基准）
  var viewW = 0, viewH = 0;     // 画布 CSS 尺寸
  var mouse = null;             // 画布内鼠标位置（CSS px）
  var lastState = null;         // 最近一次采样结果（悬浮提示用）
  var lastCaption = null;
  var connState = 'wait';       // wait=连接中 / ok=已连接 / bad=已断线
  var reconnectTimer = null;
  var reconnectAttempts = 0;    // 断线重连计数（#036：横幅"重连中(第N次)"，连上清零）
  var backoffMs = BACKOFF_INIT;
  var wsSock = null;            // 当前 WS 连接（上行意图发送用）
  var moveTarget = null;        // 点击走位目标（本地可视化；服务器才是权威，虚线只是提示）
  var attackFx = null;          // 点击出刀反馈 {x, y, t}（红框闪 300ms，前端本地效果）
  var predTarget = null;        // 自发 move 意图的预测目标 {x,y}（仅浏览器自己发的才预测）
  var predAnchor = null;        // 预测锚点 {tick,x,y,t}：最近确认的服务器坐标+拍号+确认时刻
  var predHold = null;          // 停止冻结 {x,y}：stop 后立即站住的显示位置（快照确认后回落）
  var lastRawTick = null;       // 最新原始快照拍号（预测锚定用；插值采样是延迟视图）
  var lastRawEnts = null;       // 最新原始快照实体表（服务器权威坐标）
  var attackLines = [];         // 刀光线数组 [{x0,y0,x1,y1,born,miss}]（0.3s 生命周期，画完即剔）
  var killFx = [];              // 击杀标记数组 [{x,y,born}]（1.5s 渐隐 ✕；弹丸不算击杀，期五第六刀）
  var hitFlashFx = [];          // 弹丸命中闪白数组 [{x,y,born}]（#034，300ms 白框）
  var dustFx = [];              // 哑弹落尘数组 [{x,y,born}]（期五第六刀，500ms 灰圈渐隐）
  var noReachFx = [];           // "够不着"提示数组 [{x,y,born}]（期五第六刀，1s 灰字渐隐）
  var shootArcs = [];           // 远程射击弹道弧线预览 [{pts:[{x,y}...], born}]（#036 纯装饰 1.2s 淡出）
  var projPrev = null;          // 弹丸上一快照位置表 Map(id->{x,y})（#034 尾迹方向差分用）
  var camMode = 'follow';       // 镜头模式：'follow'=跟随 P（默认）/ 'pano'=全景
  var cam = null;               // 相机中心（世界坐标，lerp 平滑）
  var camEdges = null;          // 相机贴边标记 {l,r,b,t}（viewport 边界线可视化用）
  var view = null;              // 当前可视世界窗口（全景=viewport；跟随=相机窗口）
  var terrainData = null;       // 地形折线 {x0,dx,heights}（D-016；null=平地/旧协议，回落 y=0 横线）
  var loggedOut = null;         // 已登出的处置（'take'|'stay_auto'|'stay_ai'，#025）；null=未登出
                                // 登出后连接降级观战继续直播；此时断线不自动重连（重连=抢回操控）
  var logoutPanelBusy = false;  // 弹窗决策/淡出进行中：禁一切面板交互（防双发 logout）
  var avatarAssets = [];        // D-017 资产调试面板 [{id,url,w,h,canvas}]——canvas=128x128
                                // 离屏掩码图（实心白半透明），avatar_assets 消息到达即重建

  // ---- 快照接收：入缓冲 + 帧间隔滚动平均 ----
  function onMessage(ev) {
    var snap;
    try { snap = JSON.parse(ev.data); } catch (e) {
      console.warn('[cw] 收到非 JSON 帧，丢弃');
      return;
    }
    // D-017：avatar_assets 资产消息（连接首帧，非快照）——分流处理不进快照缓冲
    if (snap && snap.type === 'avatar_assets') {
      handleAvatarAssets(snap);
      return;
    }
    if (!snap || !Array.isArray(snap.entities) || !snap.viewport) {
      console.warn('[cw] 快照字段缺失（协议不符），丢弃');
      return;
    }
    var now = performance.now();
    if (lastRecvT !== null) {
      var dt = now - lastRecvT;
      if (dt > 0 && dt < INTERVAL_MAX) {
        avgInterval = avgInterval === null ? dt : avgInterval + (dt - avgInterval) * EMA_ALPHA;
      }
    }
    lastRecvT = now;
    var ents = new Map();
    for (var i = 0; i < snap.entities.length; i++) {
      ents.set(snap.entities[i].id, snap.entities[i]);
    }
    var prevFrame = buf.length >= 1 ? buf[buf.length - 1] : null;
    buf.push({ t: now, tick: snap.tick, caption: snap.caption, viewport: snap.viewport, ents: ents });
    while (buf.length > MAX_FRAMES) buf.shift();
    // 最新原始快照（服务器权威坐标）：预测锚定与和解的数据源；viewport 同步更新
    lastRawTick = snap.tick;
    lastRawEnts = ents;
    vp = snap.viewport;
    // 地形折线（D-016）：有则换新（世界创建后地形不变，重发幂等）；缺席
    //（平地世界不发/旧协议前端）清空回落 y=0 横线
    terrainData = snap.terrain || null;
    // 击杀标记（第四刀 B2）：相邻两帧对比，消失的实体在最后坐标留渐隐 ✕
    if (prevFrame) {
      var kills = collectKills(prevFrame.ents, ents);
      for (var ki = 0; ki < kills.length; ki++) {
        killFx.push({ x: kills[ki].x, y: kills[ki].y, born: now });
      }
      while (killFx.length > 32) killFx.shift();
      // 弹丸消亡分流（期五第六刀）：附近有身体=命中闪白；无人=哑弹落尘（灰圈
      // 500ms，中性特效——落地哑弹不再冒击杀✕）
      var dis = collectProjectileDissolutions(prevFrame.ents, ents);
      for (var pi = 0; pi < dis.hits.length; pi++) {
        hitFlashFx.push({ x: dis.hits[pi].x, y: dis.hits[pi].y, born: now });
      }
      while (hitFlashFx.length > 16) hitFlashFx.shift();
      for (var di = 0; di < dis.dust.length; di++) {
        dustFx.push({ x: dis.dust[di].x, y: dis.dust[di].y, born: now });
      }
      while (dustFx.length > 16) dustFx.shift();
    }
    // 弹丸尾迹方向锚点（#034）：记录本拍弹丸位置，绘制时与插值位置差分出速度方向
    var newProjPrev = new Map();
    ents.forEach(function (e) {
      if (e.kind === 'projectile') newProjPrev.set(e.id, { x: e.x, y: e.y });
    });
    projPrev = newProjPrev;
    // 和解（第四刀 A3）：P 的服务器坐标与该 tick 的预测坐标对账
    reconcileSnapshot(snap.tick, ents, now);
    // 状态栏数字取最新帧（实时性优先；插值只作用于画面位置）
    tickEl.textContent = String(snap.tick);
    cntEl.textContent = String(snap.entities.length);
    itvEl.textContent = avgInterval === null ? '-' : String(Math.round(avgInterval));
  }

  // ---- D-017 资产调试面板：avatar_assets 消息 -> 离屏掩码图 + 状态行 + URL 日志 ----
  function handleAvatarAssets(msg) {
    var list = Array.isArray(msg.assets) ? msg.assets : [];
    var built = [];
    for (var i = 0; i < list.length; i++) {
      var a = list[i];
      // 字段护栏：id/url/掩码长度齐整才收（协议不符条目跳过不崩面板）
      if (!a || typeof a.asset_id !== 'string' || typeof a.url !== 'string' ||
          !Array.isArray(a.mask) || a.mask.length !== 128 * 128) {
        console.warn('[cw] avatar_assets 条目字段缺失，跳过');
        continue;
      }
      // 掩码画进 128x128 离屏画布（实心格=白色半透明，空心透明），
      // 绘制时 drawImage 缩到 64x64（一次成像，逐帧只做缩放拷贝）
      var off = document.createElement('canvas');
      off.width = 128;
      off.height = 128;
      var octx = off.getContext('2d');
      var img = octx.createImageData(128, 128);
      for (var p = 0; p < a.mask.length; p++) {
        if (a.mask[p] === 1) {
          img.data[p * 4] = 255;
          img.data[p * 4 + 1] = 255;
          img.data[p * 4 + 2] = 255;
          img.data[p * 4 + 3] = 128;
        }
      }
      octx.putImageData(img, 0, 0);
      built.push({ id: a.asset_id, url: a.url, w: a.w, h: a.h, canvas: off });
      console.info('[cw] 头像资产 ' + a.asset_id + ' 原图 URL: ' + location.origin + a.url);
    }
    avatarAssets = built; // 整表换新（重连重发幂等）
    assetsEl.textContent = String(built.length);
  }

  // 调试面板渲染（画布左上角）：每张掩码 64x64 + asset_id 前 8 位——
  // 肉眼验收"掩码对得上原图"；掩码挂实体渲染=后续刀，本卡不碰
  function drawAvatarPanel() {
    if (avatarAssets.length === 0) return;
    var x0 = 8, y0 = 8, size = 64, slot = size + 18;
    ctx.save();
    ctx.imageSmoothingEnabled = false; // 像素风缩放（128 -> 64 保锐利）
    ctx.font = '11px Consolas, monospace';
    ctx.textAlign = 'left';
    ctx.textBaseline = 'top';
    for (var i = 0; i < avatarAssets.length; i++) {
      var a = avatarAssets[i];
      var y = y0 + i * slot;
      ctx.drawImage(a.canvas, x0, y, size, size);
      ctx.strokeStyle = '#30363d';
      ctx.lineWidth = 1;
      ctx.strokeRect(x0 - 0.5, y - 0.5, size + 1, size + 1);
      ctx.fillStyle = '#8b949e';
      ctx.fillText(a.id.slice(0, 8), x0, y + size + 2);
    }
    ctx.restore();
  }

  function resetBuffer() {
    buf = [];
    avgInterval = null;
    lastRecvT = null;
    projPrev = null; // 弹丸尾迹锚点随缓冲清空（重连后无历史方向，尾迹从零起步）
    terrainData = null; // 地形折线随缓冲清空（下一帧快照按 terrain 字段有无重建）
    itvEl.textContent = '-';
  }

  // ---- 本地预测与和解（第四刀 A；镜像规则声明见 PURE 区 applyMoveSteps）----

  function clearPrediction() {
    predTarget = null;
    predAnchor = null;
    predHold = null;
  }

  // 和解：每帧快照到达，P 的服务器坐标与该 tick 的预测坐标对账（阈值 0.25 格）
  function reconcileSnapshot(tick, entsMap, now) {
    var me = findControlled(entsMap);
    if (!me) {
      clearPrediction(); // P 死亡/身份消失：清全部预测状态
      return;
    }
    if (predHold) {
      // 停止冻结中：服务器站定点==冻结点则确认回落插值；否则硬跟服务器
      //（stop 在途时服务器可能多走一步），直到它停稳
      if (worldDist2(predHold.x, predHold.y, me.x, me.y) < 0.0625) {
        predHold = null;
      } else {
        predHold = { x: me.x, y: me.y };
      }
      return;
    }
    if (!predTarget || !predAnchor) return;
    var steps = tick - predAnchor.tick;
    if (steps < 0) return; // 拍号倒退（不应发生）：不动预测等下一帧
    if (reconcileDecision(predAnchor.x, predAnchor.y, predTarget.x, predTarget.y, steps, me.x, me.y) === 'confirm') {
      predAnchor = { tick: tick, x: me.x, y: me.y, t: now }; // 预测确认：锚点前移
    } else {
      // 分歧（被挡/外力移动/意图未生效如观战连接）：P 弹回真实位置——
      // 清预测后渲染回落插值采样（=服务器真实坐标，玩家可见校正），
      // 本地 move 目标与虚线一并清除
      moveTarget = null;
      clearPrediction();
    }
  }

  // 建立预测（仅浏览器自己发的 move 意图调用；键盘/终端来源不预测）：
  // 锚点 = 最新原始快照的 P 服务器坐标，拍号 +1（意图下一拍 drain 消费，
  // 首个确认帧应零位移，此后每帧一步——见 net-protocol.md 预测节）
  function startPrediction(target) {
    if (lastRawTick === null || !lastRawEnts || avgInterval === null) return;
    var me = findControlled(lastRawEnts);
    if (!me) return;
    predHold = null;
    predTarget = { x: target.x, y: target.y };
    predAnchor = { tick: lastRawTick + 1, x: me.x, y: me.y, t: performance.now() };
  }

  // 停止：清预测目标；预测在途则冻结当前预测位置实现"立即站住"，
  // 待快照确认站定（或 ≤1 步的硬跟修正）后回落插值
  function stopPrediction() {
    moveTarget = null;
    if (predTarget && predAnchor) {
      var k = renderSteps(performance.now(), predAnchor.t, avgInterval || 0);
      var pp = applyMoveSteps(predAnchor.x, predAnchor.y, predTarget.x, predTarget.y, k);
      predHold = { x: pp.x, y: pp.y };
    }
    predTarget = null;
    predAnchor = null;
  }

  // 渲染延迟：1.5×平均帧间隔（覆盖一整帧的到达抖动；间隔异常时钳进合理域）
  function renderDelay() {
    if (avgInterval === null) return 0;
    var itv = Math.min(Math.max(avgInterval, INTERVAL_MIN), INTERVAL_MAX);
    return DELAY_FACTOR * itv;
  }

  // 单帧原样采样（无插值：缓冲不足两帧 / 渲染时间溢出钳边时用）
  function plainFrame(f) {
    return { caption: f.caption, viewport: f.viewport, ents: f.ents };
  }

  // 按渲染时间定位两帧并线性插值（本刀灵魂）
  function sampleState(renderTime) {
    var n = buf.length;
    if (n === 0) return null;
    if (n === 1) return plainFrame(buf[0]);
    var ai = -1;
    for (var i = n - 1; i >= 0; i--) {
      if (buf[i].t <= renderTime) { ai = i; break; }
    }
    if (ai < 0) return plainFrame(buf[0]);        // 早于最老帧（延迟突降短暂窗口）：钳最老
    if (ai >= n - 1) return plainFrame(buf[n - 1]); // 晚于最新帧（断流/暂停）：钳最新=画面冻结
    var A = buf[ai], B = buf[ai + 1];
    var span = B.t - A.t;
    var alpha = span > 0 ? Math.min(1, Math.max(0, (renderTime - A.t) / span)) : 0;
    var ents = new Map();
    B.ents.forEach(function (b, id) {
      var a = A.ents.get(id);
      if (a) {
        // 两帧同在：位置线性插值，其余字段取新帧
        ents.set(id, {
          id: id, glyph: b.glyph, color: b.color, kind: b.kind, name: b.name,
          controlled: b.controlled === true, owner: b.owner,
          disposition: b.disposition, wait_left: b.wait_left,
          x: a.x + (b.x - a.x) * alpha,
          y: a.y + (b.y - a.y) * alpha
        });
      } else {
        // 新实体：从出生帧位置直接显示（无插值起点）
        ents.set(id, b);
      }
    });
    // A 有 B 无的实体自然消失（死亡即消失，任务书定案可接受）
    return { caption: B.caption, viewport: B.viewport, ents: ents };
  }

  // ==== [PURE-BEGIN] 纯逻辑区（node 机器验证覆盖；不碰 DOM/WS/Canvas）====

  // ⚠️ 镜像规则声明（v1.5，#030 半径物理）：applyMoveSteps 与 bin/codeworld/src/input.rs 的
  // next_move_step 是同一规则的 JS/Rust 双实现——
  // 矢量步：步长恒 1.0 朝目标矢量方向（斜走一步到位，L 形先横后竖废除）；
  // 剩余距离 ≤1.0 终进近一步精确踏上目标（防到达容差带两侧振荡）；
  // 格距平方 ≤0.25（格距 ≤0.5）到达即停（地面清目标 / 空中悬停位置冻结）；
  // 落地钳半径线 0.5（镜像服务器地面约束：圆心即位置，脚贴地）。
  // 本函数建模"意图路径"（无阻挡理想轨迹）——服务器分轴滑动/被挡由和解对账
  // 纠偏（分歧瞬移回服务器真实坐标，#028 契约不变）。改一处必须同步另一处，
  // 协议契约见 docs/net-protocol.md v1.5「客户端预测与和解」节。
  var FLY_GROUND = 0.5; // 地面线 = 玩家半径（P 恒 0.5；与服务器半径线同值）

  // 从 (px,py) 向目标连走 steps 步（每步 1.0 矢量步，到达即停）；镜像 next_move_step 的
  // 循环展开版——每次迭代等价于 Rust 侧调用一次 next_move_step 的净效果
  function applyMoveSteps(px, py, tx, ty, steps) {
    var x = px, y = py;
    var k = steps;
    while (k > 0) {
      var dx = tx - x, dy = ty - y;
      var d2 = dx * dx + dy * dy;
      if (d2 <= 0.25) break;                  // 已到达：地面站住清目标 / 空中悬停（位置冻结）
      var sx, sy;
      if (d2 <= 1.0) {
        sx = dx; sy = dy;                     // 终进近：剩余 ≤1.0 一步踏上目标（防振荡）
      } else {
        var inv = 1.0 / Math.sqrt(d2);
        sx = dx * inv; sy = dy * inv;         // 矢量方向 ×1.0（斜走一步到位）
      }
      x += sx; y += sy;
      if (y < FLY_GROUND) y = FLY_GROUND;     // 落地钳半径线（镜像服务器地面约束）
      k--;
    }
    return { x: x, y: y };
  }

  function worldDist2(ax, ay, bx, by) {
    var dx = ax - bx, dy = ay - by;
    return dx * dx + dy * dy;
  }

  // 和解决策：预期位置（锚点起步 steps 步）与服务器坐标对账——
  // 距离 < 0.25 格（平方 < 0.0625）= 一致确认；否则分歧
  function reconcileDecision(ax, ay, tx, ty, steps, sx, sy) {
    var e = applyMoveSteps(ax, ay, tx, ty, steps);
    return worldDist2(e.x, e.y, sx, sy) < 0.0625 ? 'confirm' : 'diverge';
  }

  // 渲染外推步数：锚点确认后，进行中的那一拍乐观先走一步（点击瞬间 P 就动）；
  // itv 无效或时钟回拨钳 1，上限钳 256（viewport 对角约 112 步封顶留富余）
  function renderSteps(now, anchorT, itv) {
    if (!(itv > 0)) return 1;
    var k = 1 + Math.floor((now - anchorT) / itv);
    if (k < 1) k = 1;
    if (k > 256) k = 256;
    return k;
  }

  // 相机 clamp：跟随窗口 cw×ch 夹进视口域 [vx0,vx0+vw]×[vy0,vy0+vh]；
  // 窗口不小于域时居中并标记两侧贴边。返回中心 + 四边贴边标记（边界线可视化）
  function clampCamera(cx, cy, vx0, vy0, vw, vh, cw, ch) {
    var halfW = cw / 2, halfH = ch / 2;
    var x = cx, y = cy;
    var edgeL = false, edgeR = false, edgeB = false, edgeT = false;
    if (cw >= vw) {
      x = vx0 + vw / 2;
      edgeL = true;
      edgeR = true;
    } else if (x < vx0 + halfW) {
      x = vx0 + halfW;
      edgeL = true;
    } else if (x > vx0 + vw - halfW) {
      x = vx0 + vw - halfW;
      edgeR = true;
    }
    if (ch >= vh) {
      y = vy0 + vh / 2;
      edgeB = true;
      edgeT = true;
    } else if (y < vy0 + halfH) {
      y = vy0 + halfH;
      edgeB = true;
    } else if (y > vy0 + vh - halfH) {
      y = vy0 + vh - halfH;
      edgeT = true;
    }
    return { x: x, y: y, edgeL: edgeL, edgeR: edgeR, edgeB: edgeB, edgeT: edgeT };
  }

  // 击杀检测：上一帧有、新帧无的实体 = 消失（死亡/残影消散），返回最后坐标。
  // 弹丸排除（期五第六刀）：弹丸消亡（命中/落地/超时）是弹道生命周期不是击杀，
  // 落地哑弹不再冒红✕——走 collectProjectileDissolutions 的尘点/闪白分流
  function collectKills(prevEnts, newEnts) {
    var out = [];
    prevEnts.forEach(function (e, id) {
      if (e.kind === 'projectile') return;
      if (!newEnts.has(id)) out.push({ x: e.x, y: e.y });
    });
    return out;
  }

  // 贴身判距镜像（#034 attack_at 接触化；表现层提示用，服务器权威裁决）：
  // dist < r攻 + r目 + 0.5（胳膊冗余），严格 <（恰好判距=落空）。
  // 与 crates/core/src/collision.rs melee_in_reach 同判据——改一处必须同步另一处，
  // 协议锚在 net-protocol.md 弹道节（表现层镜像，漂移只影响提示不影响裁决）
  var MELEE_REACH = 0.5;
  function meleeInReach(ax, ay, ar, bx, by, br) {
    var dx = ax - bx, dy = ay - by;
    var s = ar + br + MELEE_REACH;
    return dx * dx + dy * dy < s * s;
  }

  // ---- 点击分流与远程射击（#036 期五第五刀；node 机器验证覆盖）----

  // 选中容差（期四第七刀）：点击类游戏标配的选中放宽——距活动实体圆心 <1.5 格
  // 即选中（不必点得极准）；多命中取最近；creation 仍 AABB 判定不吃容差
  //（服务器 attack 结算仍是 #031 点命中制，容差只是前端"选谁"，命中消息带实体快照坐标）
  var CLICK_TOLERANCE = 1.5;

  // 点击处的实体选中判定（#031 + 第七刀容差）：活动实体=距圆心 < CLICK_TOLERANCE；
  // creation=格对齐方块闭区间包含（原判据不动）；creation_dynamic（D-003）=圆实体
  // 同活动实体待遇（容差判定）；操控目标自己排除（点自己不出刀）；
  // 弹丸排除（#034：弹丸是飞行物不是攻击目标，服务器 find_hit_target 同判据）
  function nearestEntityAtWorld(state, wx, wy) {
    if (!state) return null;
    var hit = null, hd = 0;
    state.ents.forEach(function (e) {
      if (e.controlled === true) return;
      if (e.kind === 'projectile') return;
      var dx = e.x - wx, dy = e.y - wy;
      if (e.kind === 'creation') {
        var mx = Math.floor(e.x), my = Math.floor(e.y);
        if (!(wx >= mx && wx <= mx + 1 && wy >= my && wy <= my + 1)) return;
      } else {
        if (dx * dx + dy * dy >= CLICK_TOLERANCE * CLICK_TOLERANCE) return;
      }
      var d = dx * dx + dy * dy;
      if (!hit || d < hd) { hit = e; hd = d; }
    });
    return hit;
  }

  // ==== 操控弹炮塔解算器（期五第六刀）：服务器 bin/codeworld/src/main.rs
  // solve_launch_vector 同算法镜像（#028 镜像铁律：改一处必须同步另一处，
  // 契约锚 net-protocol.md）。离散拍物理（重力→阻力→极速钳→位移；vy<9.8
  // 首拍砸地）下连续闭式解失真，两侧都用「速度梯×角度扫描」模拟搜索——
  // 解算说能打中当且仅当世界真的命中，不哑弹 ====
  var CTRL_SHOOT_SPEED = 12;          // 基准档（驾驶档，格/拍）
  var CTRL_SHOOT_DRAG_COMP = 1.15;    // 阻力补偿（首档=12×1.15=13.8）
  var CTRL_SHOOT_MAX_EFF_SPEED = 22;  // 自适应火力上限（速度梯末档）
  var CTRL_SOLVE_TOLERANCE = 0.65;    // 命中窗口=CCD 撞体判据（r弹0.15+r身0.5）
  var CTRL_SOLVE_CLOSE_RANGE = 2.0;   // 近距一律低弧阈值
  var CTRL_SHOOT_SPEED_LADDER = [13.8, 15, 16, 17, 18, 20, 22]; // 同 Rust 梯
  var PROJ_GRAVITY = -9.8;            // 世界重力（core DefaultPhysics 默认）
  var PROJ_DRAG = 0.05;               // 世界阻力（#034 默认）
  var PROJ_BODY = 0.15;               // 弹丸身体半径（落地判定线）
  var PROJ_MASS = 1.0;                // 定装弹质量
  var PROJ_MAX_SPEED = 25;            // 世界极速
  var PROJ_MAX_AGE = 30;              // 弹丸寿命上限（拍）
  var PROJ_TRAIL = 1.5;               // 弹丸尾迹世界长度（格；0.6→1.5 期五第六刀）

  // 弹道模拟采样（镜像 main.rs solve_launch_vector 内 simulate_path：与
  // crates/physics step_projectiles 同序——先重力、后阻力、再极速钳、再位移），
  // 返回逐拍末坐标（含起点），落地 y<半径截断。
  // 【#044 子步宪法锚·弹道积分/预览采样】N=1 整步积分，步数只由弹丸状态
  // 决定（落地 y<半径 或寿命 30 拍截断）——纯状态驱动，零机器数据
  // （墙钟/CPU/帧率），同输入必然同弧线（decisions #044 / D-015）
  function simTrajectory(sx, sy, vx, vy) {
    var pts = [{ x: sx, y: sy }];
    var x = sx, y = sy;
    for (var i = 0; i < PROJ_MAX_AGE; i++) {
      vy += PROJ_GRAVITY;
      var f = 1 - PROJ_DRAG / PROJ_MASS;
      vx *= f; vy *= f;
      var sp = Math.sqrt(vx * vx + vy * vy);
      if (sp > PROJ_MAX_SPEED) { var k = PROJ_MAX_SPEED / sp; vx *= k; vy *= k; }
      x += vx; y += vy;
      pts.push({ x: x, y: y });
      if (y < PROJ_BODY) break; // 落地截断
    }
    return pts;
  }

  // 点到线段最小距离（弹道拍间是直线段，仅拍末采样会漏最近点——高弧命中
  // 窗口在弹道中段下落穿过目标处，必须全程扫描，与服务器 CCD 撞体同判据）
  function segMinDist(a, b, p) {
    var abx = b.x - a.x, aby = b.y - a.y;
    var l2 = abx * abx + aby * aby;
    var t = l2 < 1e-12 ? 0 : (((p.x - a.x) * abx + (p.y - a.y) * aby) / l2);
    t = Math.max(0, Math.min(1, t));
    return Math.hypot(p.x - a.x - abx * t, p.y - a.y - aby * t);
  }

  // 炮塔解算器（镜像 main.rs solve_launch_vector 同算法）：给定发射点/目标点，
  // 解出能命中目标的初速矢量；无解（超远/够不着）返回 null——前端不画弧线、
  // 不发 shoot 消息（服务器同判据拒发，两侧一致不哑弹）。速度梯逐档×角度域
  // 粗扫+两轮细化（低弧 [1°,45°] / 高弧 [46°,89°]，目标高于发射点取高弧、
  // 近距一律低弧），每个候选按真实物理模拟弹道全程，全程最小距离 ≤ 命中窗口
  // 即有解；低速度档优先（观感档位）
  function solveLaunchVector(sx, sy, tx, ty) {
    var dx = tx - sx, dy = ty - sy;
    var d = Math.sqrt(dx * dx + dy * dy);
    if (d < 1e-6) return null; // 零距离：贴身刀走 attack 分流
    var highArc = d >= CTRL_SOLVE_CLOSE_RANGE && dy > 0;
    var ux = dx / d;
    function missOf(v, theta) {
      var vx = v * Math.cos(theta) * ux, vy = v * Math.sin(theta);
      var pts = simTrajectory(sx, sy, vx, vy);
      var m = Infinity;
      for (var i = 0; i < pts.length - 1; i++) {
        m = Math.min(m, segMinDist(pts[i], pts[i + 1], { x: tx, y: ty }));
      }
      return m;
    }
    var deg = Math.PI / 180;
    var lo = (highArc ? 46 : 1) * deg, hi = (highArc ? 89 : 45) * deg;
    for (var li = 0; li < CTRL_SHOOT_SPEED_LADDER.length; li++) {
      var v = CTRL_SHOOT_SPEED_LADDER[li];
      var steps = 12, step0 = (hi - lo) / steps;
      var bestTheta = lo, bestMiss = missOf(v, lo);
      for (var i = 1; i <= steps; i++) {
        var th = lo + step0 * i, m = missOf(v, th);
        if (m < bestMiss) { bestMiss = m; bestTheta = th; }
      }
      var step = step0;
      for (var r = 0; r < 2; r++) {
        var lo2 = Math.max(bestTheta - step, lo), hi2 = Math.min(bestTheta + step, hi);
        for (var j = 1; j <= 8; j++) {
          var th2 = lo2 + (hi2 - lo2) * j / 8, m2 = missOf(v, th2);
          if (m2 < bestMiss) { bestMiss = m2; bestTheta = th2; }
        }
        step /= 8;
      }
      if (bestMiss <= CTRL_SOLVE_TOLERANCE) {
        return { x: v * Math.cos(bestTheta) * ux, y: v * Math.sin(bestTheta) };
      }
    }
    return null; // 全速度梯×全角度够不着
  }

  // 弹道弧线预览采样（纯装饰，实际弹道以服务器物理为准）：解算初速起步逐拍
  // 模拟（simTrajectory 同服务器步序），每拍 8 细分插值——折线变平滑真弧线
  //（5 拍弹道 ≈ 40 点）。无解返回 null（调用方不画弧线+灰字"够不着"）。
  // 【#044 子步宪法锚·表现层】细分（8 档/拍）是渲染平滑手段，纯表现层
  // 自由——永不反向影响仿真（#044 表现层铁律：世界按拍算，画面按帧画）。
  function previewTrajectory(sx, sy, tx, ty) {
    var v = solveLaunchVector(sx, sy, tx, ty);
    if (!v) return null;
    var pts = simTrajectory(sx, sy, v.x, v.y);
    var out = [];
    for (var i = 0; i < pts.length - 1; i++) {
      for (var s = 0; s < 8; s++) {
        out.push({
          x: pts[i].x + (pts[i + 1].x - pts[i].x) * s / 8,
          y: pts[i].y + (pts[i + 1].y - pts[i].y) * s / 8,
        });
      }
    }
    out.push(pts[pts.length - 1]);
    return out;
  }

  // 点击 -> 上行消息构造（#036 三分流 + 期五第六刀无解拦截）：
  // 点中实体 + 贴身距离内（< r我+r敌+0.5）= attack（贴身刀，#027/#034 语义不变）；
  // 点中实体但超出贴身 = shoot（远程射击：先本地解算——无解=shoot-blocked 不发
  // 消息，前端灰字"够不着"；有解才发，弹道参数服务器同步算）；无实体 = move。
  // 点击实体用其快照坐标（插值显示坐标，滞后可接受）
  function decideClickMessage(state, wx, wy) {
    var hit = nearestEntityAtWorld(state, wx, wy);
    if (hit) {
      var me = findControlled(state.ents);
      var inReach = !!me && meleeInReach(
        me.x, me.y, me.radius || 0.5,
        hit.x, hit.y, hit.radius || 0.5
      );
      if (inReach) {
        return { kind: 'attack', msg: { type: 'attack', x: hit.x, y: hit.y }, fx: { x: hit.x, y: hit.y }, hit: hit };
      }
      if (!me || !solveLaunchVector(me.x, me.y, hit.x, hit.y)) {
        // 解算无解（目标超出定装射程）：不发消息（服务器同判据也会拒，
        // 前置拦截省一次往返；fx 坐标供灰字提示定位）
        return { kind: 'shoot-blocked', msg: null, fx: { x: hit.x, y: hit.y }, hit: hit };
      }
      return { kind: 'shoot', msg: { type: 'shoot', x: hit.x, y: hit.y }, fx: { x: hit.x, y: hit.y }, hit: hit };
    }
    return { kind: 'move', msg: { type: 'move', x: wx, y: wy }, fx: null, hit: null };
  }

  // 断线冻结点击门（#036）：仅"已连接"允许点击产生反馈/预测——断线（bad）与
  // 连接中（wait）一律拦截：不发预测、不画虚线、P 不动（诚实的失联画面：
  // 世界停在最后一帧快照，点击不给任何本地假反馈）。已登出（观战降级）由
  // 调用方另行早退——那是正常态不是断线，世界直播照收
  function canClick(connState) {
    return connState === 'ok';
  }

  // 断线横幅文案（#036）：醒目警示 + 重连次数
  function dcBannerText(attempt) {
    return '⚠ 与世界断开——重连中(第' + attempt + '次)';
  }

  // 弹丸消亡检测（#034）：上一帧 kind=projectile、新帧没有 = 消亡（命中/落地/超时），
  // 返回最后坐标数组。落地哑弹也会触发闪白——表现层不区分消亡原因，闪到谁算谁
  function collectProjectileHits(prevEnts, newEnts) {
    var out = [];
    prevEnts.forEach(function (e, id) {
      if (e.kind === 'projectile' && !newEnts.has(id)) out.push({ x: e.x, y: e.y });
    });
    return out;
  }

  // 弹丸消失点附近的受害者（#034 命中闪白用）：非弹丸实体中距消失点 <6 取最近；
  // 操控目标也算（弹丸可以打你）。搜索半径 6 是表现层近似（审查修正）：快照只有
  // 弹丸上一拍位置（最小集无速度字段），命中点距它最远一个速度步（H 的弹 ~4-6 格），
  // 1.5 会漏掉全部正常弹速命中；6 覆盖常见弹道，代价=哑弹在 6 格内有身体时误闪一次。
  // 弹丸消亡附近没人返回 null，不闪
  function nearestProjectileVictim(entsMap, x, y) {
    var hit = null, hd = 6.0 * 6.0;
    entsMap.forEach(function (e) {
      if (e.kind === 'projectile') return;
      var dx = e.x - x, dy = e.y - y;
      var d = dx * dx + dy * dy;
      if (d >= hd) return;
      hit = e; hd = d;
    });
    return hit;
  }

  // 弹丸消亡分流（期五第六刀）：上一帧 kind=projectile、新帧没有 = 消亡。
  // 消亡点附近有身体 = 命中（返回 victim 坐标给闪白）；附近无人 = 落地/超时
  // 哑弹（返回消亡点坐标给灰尘点）——弹丸生命周期不再混进击杀✕
  function collectProjectileDissolutions(prevEnts, newEnts) {
    var hits = [], dust = [];
    prevEnts.forEach(function (e, id) {
      if (e.kind !== 'projectile' || newEnts.has(id)) return;
      var victim = nearestProjectileVictim(newEnts, e.x, e.y);
      if (victim) hits.push({ x: victim.x, y: victim.y });
      else dust.push({ x: e.x, y: e.y });
    });
    return { hits: hits, dust: dust };
  }

  // 悬浮标签锚点（期五第六刀）：贴着实体正上方水平居中（身体圆顶再留 4px），
  // 越界 clamp 视口内。px/py=实体屏幕坐标、rPx=身体圆像素半径、tipW/tipH=标签尺寸
  function tooltipAnchor(px, py, rPx, tipW, tipH, viewW) {
    return {
      left: Math.min(Math.max(px - tipW / 2, 4), viewW - tipW - 4),
      top: Math.max(py - rPx - tipH - 4, 4),
    };
  }

  // 实体表里找操控目标（controlled 标记是服务器每拍解析的 me 声明，#024）
  function findControlled(entsMap) {
    var me = null;
    entsMap.forEach(function (e) {
      if (e.controlled === true) me = e;
    });
    return me;
  }

  // 登出态跟随目标（期五第二刀）：已登出且角色留在世界（stay_auto/stay_ai）时，
  // 服务器不再标 controlled（#032：处置中的玩家不被 me 声明重认领），快照改带
  // disposition 状态签——按状态签找自己的 P；P 消失（take 带走/被杀）返回 null，
  // 镜头回落全景由调用方处理。dispose_wait 是意外断线的抢救窗口，不算登出留场
  function findDisposedPlayer(entsMap) {
    var me = null;
    entsMap.forEach(function (e) {
      if (e.kind === 'player' && (e.disposition === 'auto' || e.disposition === 'ai-await')) me = e;
    });
    return me;
  }

  // 登出处置选项 -> 大白话文案（面板/横幅共用；非法值原样兜底）
  function logoutChoiceLabel(d) {
    return { take: '带走', stay_auto: '留下 · 自主', stay_ai: '留下 · 等 AI' }[d] || d;
  }

  // 实体头顶处置标注（#025 快照 disposition 字段；null=在线态无标注）。
  // dispose_wait 带倒数（意外断线的抢救窗口），auto/ai-await 是状态签
  function disposeTag(e) {
    if (e.disposition === 'dispose_wait') {
      return '处置等待中 ' + (e.wait_left !== undefined && e.wait_left !== null ? e.wait_left : '?') + ' 拍';
    }
    if (e.disposition === 'auto') return '自主活动中';
    if (e.disposition === 'ai-await') return 'AI 待接入';
    return null;
  }

  // ==== [PURE-END] ====

  // ---- 坐标换算（net-protocol.md §4；世界 y 向上、屏幕 y 向下须翻转，负数域直接可用）----
  // 基准 = view（当前可视窗口：全景=viewport / 跟随=相机窗口），镜头切换自动适配
  function worldToScreen(wx, wy) {
    return {
      x: (wx - view.x0) / view.w * viewW,
      y: viewH - (wy - view.y0) / view.h * viewH
    };
  }
  function screenToWorld(sx, sy) {
    return {
      x: view.x0 + sx / viewW * view.w,
      y: view.y0 + (viewH - sy) / viewH * view.h
    };
  }

  // ---- 鼠标操控（#027 第三刀）：点击分流/选中判定/弹道预览等纯逻辑函数已收拢进
  // 上方 [PURE] 区（#036 起统一 node 机器验证）----

  // 上行发送（尽力而为：未连接/非操控连接时服务器侧丢弃，前端不感知不报错）
  function sendMsg(obj) {
    if (wsSock && wsSock.readyState === 1) {
      wsSock.send(JSON.stringify(obj));
    }
  }

  // ---- 画布尺寸（devicePixelRatio 适配；绘制一律用 CSS 像素）----
  function resize() {
    var dpr = window.devicePixelRatio || 1;
    var r = stage.getBoundingClientRect();
    viewW = Math.max(1, r.width);
    viewH = Math.max(1, r.height);
    cv.width = Math.round(viewW * dpr);
    cv.height = Math.round(viewH * dpr);
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  }
  window.addEventListener('resize', resize);
  resize();

  var COLOR_BG = '#0e1116';
  var COLOR_GRID = '#1b222d';
  var COLOR_GROUND = '#4b5666';

  // 每帧全量重画（60fps × 实体≤64 + 网格一次 path 画完，无压力）
  function draw(state) {
    ctx.fillStyle = COLOR_BG;
    ctx.fillRect(0, 0, viewW, viewH);
    if (!state) {
      ctx.fillStyle = '#8b949e';
      ctx.font = '14px Consolas, monospace';
      ctx.textAlign = 'center';
      ctx.textBaseline = 'middle';
      ctx.fillText('等待世界快照……（启动 start.bat 后自动连接）', viewW / 2, viewH / 2);
      drawAvatarPanel(); // D-017 调试面板（不依赖快照，资产到了就画）
      return;
    }
    var cell = Math.min(viewW / view.w, viewH / view.h);

    // 背景网格：世界整数坐标处一条暗线（格子感，铺满当前可视窗口）
    ctx.strokeStyle = COLOR_GRID;
    ctx.lineWidth = 1;
    ctx.beginPath();
    var xi;
    for (xi = Math.ceil(view.x0); xi <= Math.floor(view.x0 + view.w); xi++) {
      var sx = Math.round(worldToScreen(xi, 0).x) + 0.5;
      ctx.moveTo(sx, 0);
      ctx.lineTo(sx, viewH);
    }
    var yi;
    for (yi = Math.ceil(view.y0); yi <= Math.floor(view.y0 + view.h); yi++) {
      var sy = Math.round(worldToScreen(0, yi).y) + 0.5;
      ctx.moveTo(0, sy);
      ctx.lineTo(viewW, sy);
    }
    ctx.stroke();

    // 地面线（D-016）：有地形折线数据时画高度折线（纯线起步定案，无填充/纹理）；
    // 无数据（平地/旧协议前端兼容）保留旧 y=0 横线行为
    if (terrainData && Array.isArray(terrainData.heights) && terrainData.heights.length > 1) {
      var hs = terrainData.heights;
      ctx.strokeStyle = COLOR_GROUND;
      ctx.lineWidth = 2;
      ctx.beginPath();
      for (var ti = 0; ti < hs.length; ti++) {
        var tp = worldToScreen(terrainData.x0 + ti * terrainData.dx, hs[ti]);
        if (ti === 0) ctx.moveTo(tp.x, tp.y); else ctx.lineTo(tp.x, tp.y);
      }
      ctx.stroke();
    } else if (view.y0 <= 0 && 0 <= view.y0 + view.h) {
      var gy = Math.round(worldToScreen(0, 0).y) + 0.5;
      ctx.strokeStyle = COLOR_GROUND;
      ctx.beginPath();
      ctx.moveTo(0, gy);
      ctx.lineTo(viewW, gy);
      ctx.stroke();
    }

    // 世界边界可视化（第四刀 C2）：viewport 域边缘落在画面内（相机贴边）时画界线，
    // 玩家由此知道世界到头了；全景模式边界=画布边缘不画
    if (camEdges && vp) {
      ctx.strokeStyle = '#d29922';
      ctx.lineWidth = 2;
      ctx.beginPath();
      if (camEdges.l) {
        var blx = Math.round(worldToScreen(vp.x0, 0).x) + 1;
        ctx.moveTo(blx, 0);
        ctx.lineTo(blx, viewH);
      }
      if (camEdges.r) {
        var brx = Math.round(worldToScreen(vp.x0 + vp.w, 0).x) - 1;
        ctx.moveTo(brx, 0);
        ctx.lineTo(brx, viewH);
      }
      if (camEdges.b) {
        var bby = Math.round(worldToScreen(0, vp.y0).y) - 1;
        ctx.moveTo(0, bby);
        ctx.lineTo(viewW, bby);
      }
      if (camEdges.t) {
        var bty = Math.round(worldToScreen(0, vp.y0 + vp.h).y) + 1;
        ctx.moveTo(0, bty);
        ctx.lineTo(viewW, bty);
      }
      ctx.stroke();
    }

    // 实体：glyph 居中画在插值后的连续世界坐标上；活动实体按 radius 画身体圆
    //（#030：圆圈大小=身体，碰撞即此圈）；creation 画格对齐方形底衬（地址格）
    ctx.textAlign = 'center';
    ctx.textBaseline = 'middle';
    ctx.font = Math.round(Math.min(32, Math.max(10, cell * 0.9))) + 'px Consolas, monospace';
    var half = cell * 0.45;
    state.ents.forEach(function (e) {
      var p = worldToScreen(e.x, e.y);
      if (p.x < -cell || p.x > viewW + cell || p.y < -cell || p.y > viewH + cell) return; // 视口外剔除
      if (e.kind === 'projectile') {
        // 弹丸（#034）：小圆点 + 速度方向短尾迹（颜色=发射者 glyph 色，快照 color 字段）
        var pr = Math.max(3, cell * Math.max(e.radius, 0.12));
        var prevP = projPrev ? projPrev.get(e.id) : null;
        var dxv = prevP ? e.x - prevP.x : 0, dyv = prevP ? e.y - prevP.y : 0;
        var vl = Math.sqrt(dxv * dxv + dyv * dyv);
        if (vl > 0.01) {
          var tl = PROJ_TRAIL; // 尾迹世界长度（格，PURE 区常量），方向 = 当前插值位置 - 上一快照位置
          var ts = worldToScreen(e.x - dxv / vl * tl, e.y - dyv / vl * tl);
          ctx.strokeStyle = e.color || '#e6edf3';
          ctx.lineWidth = Math.max(1.5, pr * 0.8);
          ctx.globalAlpha = 0.5;
          ctx.lineCap = 'round';
          ctx.beginPath();
          ctx.moveTo(ts.x, ts.y);
          ctx.lineTo(p.x, p.y);
          ctx.stroke();
          ctx.globalAlpha = 1;
        }
        ctx.fillStyle = e.color || '#e6edf3';
        ctx.beginPath();
        ctx.arc(p.x, p.y, pr, 0, Math.PI * 2);
        ctx.fill();
        return; // 弹丸不画 glyph/身体圆（小圆点即身体）
      }
      if (e.kind === 'creation') {
        ctx.fillStyle = 'rgba(255,255,255,0.10)'; // 造物方形底衬
        ctx.fillRect(p.x - half, p.y - half, half * 2, half * 2);
      } else if (e.radius > 0) {
        // 活动实体 + creation_dynamic（D-003 全活造物）：按 radius 画身体圆
        ctx.strokeStyle = 'rgba(255,255,255,0.16)'; // 身体圆轮廓（半径可视化，网格线保留地址感）
        ctx.lineWidth = 1;
        ctx.beginPath();
        ctx.arc(p.x, p.y, Math.max(cell * e.radius, 4), 0, Math.PI * 2);
        ctx.stroke();
      }
      if (e.controlled === true) {
        ctx.strokeStyle = '#ffffff';              // 操控目标白色描边框
        ctx.lineWidth = 2;
        ctx.strokeRect(p.x - half - 2, p.y - half - 2, half * 2 + 4, half * 2 + 4);
      }
      ctx.fillStyle = e.color;
      ctx.fillText(e.glyph, p.x, p.y);
    });

    // 处置状态头顶标注（#025 期五第一刀）：处置等待=黄色倒数（抢救窗口），
    // 自主活动中 / AI 待接入=灰色状态签。第二遍画避免污染实体字号
    ctx.font = '12px Consolas, monospace';
    state.ents.forEach(function (e) {
      var tag = disposeTag(e);
      if (!tag) return;
      var p = worldToScreen(e.x, e.y);
      if (p.x < -cell || p.x > viewW + cell || p.y < -cell || p.y > viewH + cell) return;
      ctx.fillStyle = e.disposition === 'dispose_wait' ? '#d29922' : '#8b949e';
      ctx.fillText(tag, p.x, p.y - half - 12);
    });

    // 点击走位可视化（#027）：操控实体 -> 目标 虚线 + 目标点标记（本地提示，服务器权威；
    // 操控实体取的是预测后的渲染位置，虚线跟着预测走）。
    // 空中目标（第五刀）：目标向地面投影一条垂直参考线 + 地面落点小十字——高度感，
    // 玩家由此看出"这是往天上飞"以及悬停点相对地面的位置
    if (moveTarget && lastState) {
      var meEnt = findControlled(lastState.ents);
      if (meEnt) {
        var pa = worldToScreen(meEnt.x, meEnt.y);
        var pb = worldToScreen(moveTarget.x, moveTarget.y);
        if (moveTarget.y > FLY_GROUND) {
          var pg = worldToScreen(moveTarget.x, 0);
          ctx.strokeStyle = 'rgba(63,185,80,0.45)';
          ctx.lineWidth = 1;
          ctx.setLineDash([2, 4]);
          ctx.beginPath();
          ctx.moveTo(pb.x, pb.y);
          ctx.lineTo(pg.x, pg.y);
          ctx.stroke();
          ctx.setLineDash([]);
          var cs = 5;
          ctx.beginPath();
          ctx.moveTo(pg.x - cs, pg.y - cs);
          ctx.lineTo(pg.x + cs, pg.y + cs);
          ctx.moveTo(pg.x + cs, pg.y - cs);
          ctx.lineTo(pg.x - cs, pg.y + cs);
          ctx.stroke();
        }
        ctx.strokeStyle = '#3fb950';
        ctx.lineWidth = 1.5;
        ctx.setLineDash([5, 5]);
        ctx.beginPath();
        ctx.moveTo(pa.x, pa.y);
        ctx.lineTo(pb.x, pb.y);
        ctx.stroke();
        ctx.setLineDash([]);
        ctx.strokeRect(pb.x - 6, pb.y - 6, 12, 12);
      }
    }
    // 点击出刀反馈：目标格红框闪 300ms（贴身够得着）；#034 落空（判距不够）= 灰框
    //（前端本地效果；服务器结算下一帧快照自然呈现——接触门在 deliver，灰框只是预告）
    if (attackFx) {
      var age = performance.now() - attackFx.t;
      if (age > 300) {
        attackFx = null;
      } else {
        var pf = worldToScreen(attackFx.x, attackFx.y);
        ctx.strokeStyle = attackFx.miss ? '#8b949e' : '#f85149';
        ctx.lineWidth = 2;
        ctx.globalAlpha = 1 - age / 300;
        ctx.strokeRect(pf.x - half - 3, pf.y - half - 3, (half + 3) * 2, (half + 3) * 2);
        ctx.globalAlpha = 1;
      }
    }

    // 刀光线（第四刀 B1）：P 与目标间一道快速淡出的攻击线（0.3s 生命周期，超时即剔）；
    // #034 落空刀=灰线（够不着，物理诚实）
    var nowFx = performance.now();
    attackLines = attackLines.filter(function (l) { return nowFx - l.born < 300; });
    for (var li = 0; li < attackLines.length; li++) {
      var al = attackLines[li];
      var p0 = worldToScreen(al.x0, al.y0);
      var p1 = worldToScreen(al.x1, al.y1);
      ctx.globalAlpha = 1 - (nowFx - al.born) / 300;
      ctx.strokeStyle = al.miss ? '#8b949e' : '#f85149';
      ctx.lineWidth = 2.5;
      ctx.beginPath();
      ctx.moveTo(p0.x, p0.y);
      ctx.lineTo(p1.x, p1.y);
      ctx.stroke();
      ctx.globalAlpha = 1;
    }

    // 弹丸命中闪白（#034）：被撞实体白框闪 300ms（快照驱动的本地特效）
    hitFlashFx = hitFlashFx.filter(function (f) { return nowFx - f.born < 300; });
    for (var hi = 0; hi < hitFlashFx.length; hi++) {
      var hf = worldToScreen(hitFlashFx[hi].x, hitFlashFx[hi].y);
      ctx.globalAlpha = 1 - (nowFx - hitFlashFx[hi].born) / 300;
      ctx.strokeStyle = '#ffffff';
      ctx.lineWidth = 2;
      ctx.strokeRect(hf.x - half - 3, hf.y - half - 3, (half + 3) * 2, (half + 3) * 2);
      ctx.globalAlpha = 1;
    }

    // 远程射击弹道弧线预览（#036）：本地模拟的定装弹虚线弧+终点小圆（纯装饰，
    // 实际弹道以服务器物理为准），1.2s 渐隐
    shootArcs = shootArcs.filter(function (a) { return nowFx - a.born < 1200; });
    for (var si = 0; si < shootArcs.length; si++) {
      var arc = shootArcs[si];
      ctx.globalAlpha = 1 - (nowFx - arc.born) / 1200;
      ctx.strokeStyle = '#d29922';
      ctx.lineWidth = 1.5;
      ctx.setLineDash([4, 4]);
      ctx.beginPath();
      for (var pi = 0; pi < arc.pts.length; pi++) {
        var ap = worldToScreen(arc.pts[pi].x, arc.pts[pi].y);
        if (pi === 0) ctx.moveTo(ap.x, ap.y); else ctx.lineTo(ap.x, ap.y);
      }
      ctx.stroke();
      ctx.setLineDash([]);
      var lastPt = arc.pts[arc.pts.length - 1];
      var lp = worldToScreen(lastPt.x, lastPt.y);
      ctx.beginPath();
      ctx.arc(lp.x, lp.y, 4, 0, Math.PI * 2);
      ctx.stroke();
      ctx.globalAlpha = 1;
    }

    // 哑弹落尘（期五第六刀）：弹丸落地/超时消亡处灰圈扩散渐隐 500ms（中性特效，
    // 不再冒击杀✕）
    dustFx = dustFx.filter(function (f) { return nowFx - f.born < 500; });
    for (var ui = 0; ui < dustFx.length; ui++) {
      var du = dustFx[ui];
      var dp = worldToScreen(du.x, du.y);
      var age = (nowFx - du.born) / 500;
      ctx.globalAlpha = (1 - age) * 0.6;
      ctx.strokeStyle = '#8b949e';
      ctx.lineWidth = 1.5;
      ctx.beginPath();
      ctx.arc(dp.x, dp.y, 3 + age * 7, 0, Math.PI * 2);
      ctx.stroke();
      ctx.globalAlpha = 1;
    }

    // "够不着"提示（期五第六刀）：射击解算无解的目标旁灰字 1s 渐隐（不打哑弹）
    noReachFx = noReachFx.filter(function (f) { return nowFx - f.born < 1000; });
    for (var ni = 0; ni < noReachFx.length; ni++) {
      var nr = noReachFx[ni];
      var np = worldToScreen(nr.x, nr.y);
      ctx.globalAlpha = 1 - (nowFx - nr.born) / 1000;
      ctx.fillStyle = '#8b949e';
      ctx.font = '12px Consolas, monospace';
      ctx.textAlign = 'center';
      ctx.fillText('够不着', np.x, np.y - cell * 0.8);
      ctx.globalAlpha = 1;
    }

    // 击杀标记（第四刀 B2）：快照中消失的实体在最后坐标留渐隐 ✕（1.5s；弹丸
    // 已被 collectKills 排除——哑弹落尘/命中闪白各走各的特效）
    killFx = killFx.filter(function (m) { return nowFx - m.born < 1500; });
    for (var mi = 0; mi < killFx.length; mi++) {
      var km = killFx[mi];
      var pk = worldToScreen(km.x, km.y);
      var kr = Math.max(6, cell * 0.3);
      ctx.globalAlpha = 1 - (nowFx - km.born) / 1500;
      ctx.strokeStyle = '#f85149';
      ctx.lineWidth = 2;
      ctx.beginPath();
      ctx.moveTo(pk.x - kr, pk.y - kr);
      ctx.lineTo(pk.x + kr, pk.y + kr);
      ctx.moveTo(pk.x + kr, pk.y - kr);
      ctx.lineTo(pk.x - kr, pk.y + kr);
      ctx.stroke();
      ctx.globalAlpha = 1;
    }
    drawAvatarPanel(); // D-017 资产调试面板（最后画=置顶，实体可能压过左上角）
  }

  function updateCaption(state) {
    var text = state ? state.caption : '（未连接世界）';
    if (text !== lastCaption) {
      lastCaption = text;
      captionEl.textContent = text;
    }
  }

  // ---- 悬浮提示 + 鼠标世界坐标（第三刀点击操控的直观校验）----
  function updateTooltip() {
    var hit = null;
    if (mouse && lastState && lastState.ents.size > 0 && view) {
      var cell = Math.min(viewW / view.w, viewH / view.h);
      var thr = Math.max(10, cell * 0.6);
      var thr2 = thr * thr;
      lastState.ents.forEach(function (e) {
        var p = worldToScreen(e.x, e.y);
        var d = (p.x - mouse.x) * (p.x - mouse.x) + (p.y - mouse.y) * (p.y - mouse.y);
        if (d <= thr2 && (!hit || d < hit.d)) hit = { e: e, p: p, d: d };
      });
    }
    if (!hit) {
      tipEl.hidden = true;
      return;
    }
    var e = hit.e;
    var text = e.name + ' · ' + e.kind;
    if (e.controlled) text += ' · 操控中';
    if (e.kind === 'creation' && e.owner) text += ' · owner ' + e.owner.slice(0, 8);
    if (e.kind === 'creation_dynamic' && e.owner) text += ' · 全活造物 · owner ' + e.owner.slice(0, 8);
    tipEl.textContent = text;
    tipEl.hidden = false;
    // 锚点=实体正上方水平居中（身体圆顶再留 4px，越界 clamp——期五第六刀；
    // 旧版锚鼠标指针偏移，实体移动时标签漂移）
    var cellT = Math.min(viewW / view.w, viewH / view.h);
    var rPx = Math.max(6, cellT * (e.radius || 0.5));
    var anch = tooltipAnchor(hit.p.x, hit.p.y, rPx, tipEl.offsetWidth, tipEl.offsetHeight, viewW);
    tipEl.style.left = anch.left + 'px';
    tipEl.style.top = anch.top + 'px';
  }

  cv.addEventListener('mousemove', function (ev) {
    var r = cv.getBoundingClientRect();
    mouse = { x: ev.clientX - r.left, y: ev.clientY - r.top };
    if (view) {
      var w = screenToWorld(mouse.x, mouse.y);
      mouseEl.textContent = 'x=' + w.x.toFixed(1) + ', y=' + w.y.toFixed(1);
    }
  });
  cv.addEventListener('mouseleave', function () {
    mouse = null;
    mouseEl.textContent = 'x=- , y=-';
    tipEl.hidden = true;
  });

  // ---- 点击操控（#027/#036）：左键 move/attack(近)/shoot(远)、右键 stop、S 键 stop、F 切镜头 ----
  cv.addEventListener('click', function (ev) {
    // #036 断线冻结：已登出=观战（上行被服务器丢弃）；断线/连接中=预测冻结——
    // 两种情况都不给本地假反馈（不发预测、不画虚线、P 不动）
    if (loggedOut || !canClick(connState)) return;
    if (!view || !lastState) return;
    var r = cv.getBoundingClientRect();
    var w = screenToWorld(ev.clientX - r.left, ev.clientY - r.top);
    var decision = decideClickMessage(lastState, w.x, w.y);
    if (decision.msg) sendMsg(decision.msg); // shoot-blocked 无消息可发（期五第六刀）
    if (decision.kind === 'move') {
      moveTarget = { x: w.x, y: w.y }; // 本地虚线目标（服务器权威，仅提示）
      startPrediction(moveTarget);     // 己方预测（第四刀 A1）：点击瞬间 P 就动
      return;
    }
    var meAtk = findControlled(lastState.ents);
    if (decision.kind === 'attack') {
      // 贴身刀（#027/#034）：红框闪 + 刀光线（decideClickMessage 已保证本地贴身判距内；
      // 服务器裁决仍是权威——实体若在插值滞后窗口里走开，落空由世界呈现）
      attackFx = { x: decision.fx.x, y: decision.fx.y, t: performance.now(), miss: false };
      if (meAtk) {
        attackLines.push({ x0: meAtk.x, y0: meAtk.y, x1: decision.fx.x, y1: decision.fx.y, born: performance.now(), miss: false });
        if (attackLines.length > 8) attackLines.shift();
      }
      return;
    }
    // 远程射击（#036 + 期五第六刀）：shoot-blocked=解算无解，灰字"够不着"提示、
    // 不发消息不画弧；shoot=定装弹道弧线预览（本地模拟纯装饰，实际弹道以服务器
    // 为准）——从 P 当前快照坐标到目标的一道渐隐虚线弧 + 终点小圆
    if (decision.kind === 'shoot-blocked') {
      noReachFx.push({ x: decision.fx.x, y: decision.fx.y, born: performance.now() });
      while (noReachFx.length > 8) noReachFx.shift();
      return;
    }
    if (meAtk) {
      shootArcs.push({
        pts: previewTrajectory(meAtk.x, meAtk.y, decision.fx.x, decision.fx.y),
        born: performance.now(),
      });
      while (shootArcs.length > 8) shootArcs.shift();
    }
  });
  cv.addEventListener('contextmenu', function (ev) {
    ev.preventDefault(); // 屏蔽浏览器默认菜单
    sendMsg({ type: 'stop' });
    stopPrediction();
  });
  window.addEventListener('keydown', function (ev) {
    if (ev.key === 's' || ev.key === 'S') {
      sendMsg({ type: 'stop' });
      stopPrediction();
    }
    if (ev.key === 'f' || ev.key === 'F') {
      toggleCamera();
    }
    // 登出处置（#025）：L 键/底部按钮开面板，Esc 取消
    if (ev.key === 'l' || ev.key === 'L') {
      showLogoutPanel();
    }
    if (ev.key === 'Escape') {
      cancelLogoutPanel(); // Esc = 取消（弹窗开着且非决策/淡出中才生效）
    }
  });
  camBtnEl.addEventListener('click', toggleCamera);

  // ---- 登出处置（#025 期五第一刀；期五第二刀决策即关门）----
  // 弹窗是一次决策的场所：点任一选项/取消/Esc = 决策完成 = 弹窗立刻淡出关门，
  // 决策结果交给非阻塞的横幅展示；busy 守卫保证一次决策只发一条 logout
  function showLogoutPanel() {
    if (loggedOut || logoutPanelBusy) return; // 已登出不能再登出；决策/淡出中不重开
    resetLogoutPanelVisual();
    logoutPanelEl.hidden = false;
  }

  // 开面板/关门后清掉按下回执与淡出残留（防复用残留样式）
  function resetLogoutPanelVisual() {
    logoutPanelEl.classList.remove('closing');
    Array.prototype.forEach.call(
      document.querySelectorAll('#logoutPanel .disp'),
      function (btn) { btn.classList.remove('picked'); }
    );
  }

  // 0.25s 透明淡出后收起（决策即关门）；afterFade = 关门后的收尾（如亮横幅）
  function fadeOutLogoutPanel(afterFade) {
    logoutPanelEl.classList.add('closing'); // CSS transition 渐隐 + pointer-events:none
    setTimeout(function () {
      logoutPanelEl.hidden = true;
      resetLogoutPanelVisual();
      logoutPanelBusy = false;
      if (afterFade) afterFade();
    }, PANEL_FADE_MS);
  }

  // 取消/Esc：同款淡出关门，不发任何消息
  function cancelLogoutPanel() {
    if (logoutPanelEl.hidden || logoutPanelBusy) return;
    logoutPanelBusy = true;
    fadeOutLogoutPanel(null);
  }

  // 选项落地：卡片按下高亮一拍（视觉回执）→ 发 logout 消息（服务器释放操控令牌，
  // 本连接降级观战——世界继续直播，P 的自主活动照常可见）→ 弹窗 0.25s 淡出关门
  // → 横幅接管（重新进入世界入口；重连=重新认领操控）
  Array.prototype.forEach.call(
    document.querySelectorAll('#logoutPanel .disp'),
    function (btn) {
      btn.addEventListener('click', function () {
        if (logoutPanelBusy || loggedOut) return; // 决策/淡出中禁重复点击（防双发 logout）
        var d = btn.getAttribute('data-d');
        logoutPanelBusy = true;
        btn.classList.add('picked');
        setTimeout(function () {
          loggedOut = d; // 与发消息同拍置位：回执窗口内断线仍走自动重连（意外断开语义）
          sendMsg({ type: 'logout', disposition: d });
          moveTarget = null;
          clearPrediction(); // 已无操控：预测态清空（重进后从服务器快照重来）
          fadeOutLogoutPanel(function () {
            bannerTextEl.textContent = '已登出（处置：' + logoutChoiceLabel(d) + '）· 世界直播中';
            logoutBannerEl.hidden = false;
          });
        }, PANEL_ACK_MS);
      });
    }
  );
  logoutBtnEl.addEventListener('click', showLogoutPanel);
  logoutCancelEl.addEventListener('click', cancelLogoutPanel);

  // 重新进入世界：关掉观战连接 -> 全新连接（第一连接=操控端，重新认领）。
  // take 处置下服务器会重生角色；stay_* 处置下直接抢回操控（接管者被顶掉）
  reenterBtnEl.addEventListener('click', function () {
    loggedOut = null;
    logoutBannerEl.hidden = true;
    dcBannerEl.hidden = true; // #036：重连路径重建横幅状态（连上隐藏/失败重现）
    if (wsSock) {
      var old = wsSock;
      wsSock = null;
      old.onclose = null; // 老连接的断开回调作废（防调度自动重连）
      try { old.close(); } catch (e) { /* 已断则忽略 */ }
    }
    connect();
  });

  // 镜头切换（第四刀 C3）：跟随/全景；P 不存在时自动回落全景（loop 里生效）
  function toggleCamera() {
    camMode = camMode === 'follow' ? 'pano' : 'follow';
    camBtnEl.textContent = '镜头：' + (camMode === 'follow' ? '跟随' : '全景');
  }

  // ---- 连接状态机：连接中 / 已连接 / 已断线（指数退避重连，单一定时器防死循环）----
  var CONN_TEXT = { wait: '连接中……', ok: '已连接', bad: '已断线，等待重连' };
  function renderConn() {
    var text = CONN_TEXT[connState];
    if (connState === 'bad') text += '（' + Math.round(backoffMs / 1000) + 's 后重试）';
    connEl.textContent = text;
    connEl.className = connState;
  }

  function connect() {
    if (reconnectTimer !== null) {
      clearTimeout(reconnectTimer);
      reconnectTimer = null;
    }
    connState = 'wait';
    renderConn();
    var proto = location.protocol === 'https:' ? 'wss://' : 'ws://';
    var ws = new WebSocket(proto + location.host + '/ws');
    ws.onopen = function () {
      console.info('[cw] WS 已连接');
      wsSock = ws; // 上行意图通道就绪（第一连接=操控端；后来连接上行被服务器丢弃）
      connState = 'ok';
      backoffMs = BACKOFF_INIT; // 连上了就重置退避
      reconnectAttempts = 0;    // #036：重连成功计数清零
      resetBuffer();            // 重连后清空缓冲重新开始（断线期间旧帧与新世界不连续）
      moveTarget = null;        // 断线期间旧目标失效（重连后重新点击）
      clearPrediction();        // 预测状态一并清空（重连后世界可能重启，坐标不连续）
      attackLines = [];
      killFx = [];
      dustFx = [];
      noReachFx = [];
      shootArcs = [];           // 弧线预览一并清（#036：重连后从服务器快照重来）
      dcBannerEl.hidden = true; // #036：断线横幅消失，操控恢复
      renderConn();
    };
    ws.onmessage = onMessage;
    ws.onerror = function () {
      // onclose 会跟进来，重连统一在 onclose 调度（防双调度）
      console.info('[cw] WS 连接错误（服务未启动或端口不对）');
    };
    ws.onclose = function () {
      console.info('[cw] WS 断开，' + Math.round(backoffMs / 1000) + 's 后重连');
      wsSock = null;
      moveTarget = null;
      clearPrediction(); // 断线清预测（重连后从服务器快照重新开始）——#036 冻结的根基
      attackLines = [];
      killFx = [];
      dustFx = [];
      noReachFx = [];
      shootArcs = [];
      connState = 'bad';
      // 已登出（#025）：不自动重连——重连=重新认领操控（顶掉接管者），
      // 这是"重新进入世界"按钮的显式动作；横幅保留，连接状态如实显示
      if (loggedOut) {
        connEl.textContent = '已登出 · 连接已断（点「重新进入世界」回来）';
        connEl.className = 'bad';
        return;
      }
      // #036 断线横幅（预测冻结警示）：与世界失联=点击无效（本地预测已清，
      // 画面停在最后一帧快照），顶栏醒目提示重连进度
      reconnectAttempts++;
      dcBannerTextEl.textContent = dcBannerText(reconnectAttempts);
      dcBannerEl.hidden = false;
      renderConn();
      if (reconnectTimer !== null) return; // 防双调度（onerror 后紧跟 onclose）
      reconnectTimer = setTimeout(function () {
        reconnectTimer = null;
        connect();
      }, backoffMs);
      backoffMs = Math.min(backoffMs * 2, BACKOFF_MAX);
    };
  }

  // ---- 主循环：rAF 驱动，每帧重采样重画 ----
  function loop() {
    var nowMs = performance.now();
    var state = sampleState(nowMs - renderDelay());
    lastState = state;
    // 己方预测（第四刀 A1）：P 的渲染位置用本地预测/停住冻结覆盖插值采样（零等待），
    // 他人实体照旧走插值——Valve 标准分工；对账与校正在 onMessage/reconcileSnapshot
    var meRendered = null;
    if (state) {
      var meEnt = findControlled(state.ents);
      if (!meEnt && loggedOut) {
        // 登出态镜头归属（期五第二刀）：角色留在世界（自主/AI 待命）就继续跟它
        //（快照带 disposition 状态签），P 消失（带走/被杀）保持 null → 回落全景
        meEnt = findDisposedPlayer(state.ents);
      }
      if (meEnt) {
        if (predHold) {
          meEnt.x = predHold.x; // 停止冻结：立即站住
          meEnt.y = predHold.y;
          meRendered = predHold;
        } else if (predTarget && predAnchor) {
          var k = renderSteps(nowMs, predAnchor.t, avgInterval || 0);
          var pp = applyMoveSteps(predAnchor.x, predAnchor.y, predTarget.x, predTarget.y, k);
          meEnt.x = pp.x;
          meEnt.y = pp.y;
          meRendered = pp;
        } else {
          meRendered = { x: meEnt.x, y: meEnt.y };
        }
      }
    }
    // 走位虚线到达自清（服务器到站判定才是权威，这里保持本地提示一致）
    if (moveTarget) {
      if (!meRendered) {
        moveTarget = null; // 操控实体消失（死亡/下线）：虚线失效
      } else if (worldDist2(meRendered.x, meRendered.y, moveTarget.x, moveTarget.y) <= 0.25) {
        moveTarget = null;
      }
    }
    // 镜头（第四刀 C）：跟随=lerp 对准 P + viewport 域 clamp（贴边标记→边界线）；
    // 全景=整 viewport 铺满；P 不存在（未连/死亡/观战）自动回落全景
    camEdges = null;
    if (vp) {
      if (camMode === 'follow' && meRendered) {
        if (!cam) {
          cam = { x: meRendered.x, y: meRendered.y }; // 首帧直接对准，不从原点滑移
        } else {
          cam.x += (meRendered.x - cam.x) * CAM_LERP;
          cam.y += (meRendered.y - cam.y) * CAM_LERP;
        }
        var cw = vp.w / CAM_ZOOM, ch = vp.h / CAM_ZOOM;
        var cc = clampCamera(cam.x, cam.y, vp.x0, vp.y0, vp.w, vp.h, cw, ch);
        camEdges = { l: cc.edgeL, r: cc.edgeR, b: cc.edgeB, t: cc.edgeT };
        view = { x0: cc.x - cw / 2, y0: cc.y - ch / 2, w: cw, h: ch };
      } else {
        view = vp;
      }
    }
    updateCaption(state);
    draw(state);
    updateTooltip();
    requestAnimationFrame(loop);
  }

  connect();
  requestAnimationFrame(loop);
})();