/**
 * 页面：造物工坊（D-022，workshop.html 的全部逻辑）
 * 功能：Monaco 编辑器（WAT 高亮 / 编译错红波浪线 / 自动补全）+ 造物管理
 *      （角色文件列表 / 在场造物 / 新建造物+皮上传 / 外桥只读）+ 事件流面板。
 * 流程：1. 连 WS（无 token=观战连接，收 player_roster + 每拍快照的
 *          player_events/entities——工坊不需要操控权）
 *       2. 左列表点选角色 -> GET /api/player-code 载入编辑器
 *       3. Ctrl+S / 保存按钮 -> POST /api/player-code（写 players/<名>.wat，
 *          新文件=世界下拍目录轮询自动进场；在线角色=下次进场生效）
 *       4. compile_failed -> 正则解析行号（wat 错误格式 `--> <anon>:行:列`）
 *          -> Monaco markers 红波浪线；运行错面板点击跳转导出函数所在行
 * 降级：Monaco 加载失败（vendor 缺失/超时）-> textarea + 顶部黄条，保存/加载/
 *      删除/事件流功能不炸（它们走 HTTP/WS，不依赖编辑器）。
 * 边界（卡面声明）：真·断点/单步/回放不做（违拍制全局心跳，另卡）；
 *      在线热替换不做（保存=下次进场生效）；装戴 API 未开放（皮只入库）。
 */

(function () {
  'use strict';

  // ---- 常数 ----
  var MONACO_TIMEOUT_MS = 8000;     // Monaco 加载兜底超时（vendor 缺失时不再干等）
  var PANEL_ACK_MS = 120;           // 确认弹窗按下回执时长（同 index.html 登出面板）
  var PANEL_FADE_MS = 250;          // 确认弹窗 0.25s 淡出（决策即关门标准）
  var WS_BACKOFF_INIT_MS = 1000;    // WS 重连退避起步
  var WS_BACKOFF_MAX_MS = 30000;    // WS 重连退避封顶
  var EVENT_STREAM_CAP = 200;       // 事件流行数上限（防长跑内存涨）
  var ENTITY_EVENT_KEEP = 20;       // 每造物事件缓存条数上限（展开面板取最近 3 条）

  // 保存/删除拒绝码 -> 人话（与 api.md 码表同源；服务器侧 wire 值）
  var SAVE_REJECT_TEXT = {
    NAME_EMPTY: '名字不能为空',
    NAME_TOO_LONG: '名字太长（最多 24 字符）',
    NAME_BAD_CHAR: '名字只能用字母/数字/中文/_/-（拒路径分隔符和点号）',
    CODE_TOO_LONG: '代码超过 256KB 上限',
    IO_ERROR: '服务器写盘失败（看世界日志）',
    BAD_JSON: '请求格式错误（浏览器侧 bug？）'
  };
  var DELETE_REJECT_TEXT = {
    NAME_EMPTY: '名字不能为空',
    NAME_TOO_LONG: '名字太长',
    NAME_BAD_CHAR: '非法名字',
    NAME_UNKNOWN: '角色不存在（可能已被删除）',
    IO_ERROR: '服务器删除失败（看世界日志）'
  };

  // 事件 kind -> 四色分类（err 红 / warn 黄 / info 灰 / log 白）
  var EVENT_CLASS = {
    compile_failed: 'err',
    runtime_error: 'err',
    create_player_rejected: 'err',
    select_player_rejected: 'err',
    spawn_rejected: 'warn',
    died: 'warn',
    born: 'info',
    player_joined: 'info',
    player_selected: 'info',
    player_log: 'log'
  };
  var EVENT_LABEL = {
    compile_failed: '编译失败',
    runtime_error: '运行错',
    spawn_rejected: '造物被拒',
    born: '出生',
    died: '死亡',
    player_log: '输出',
    player_joined: '进场',
    player_selected: '选角',
    create_player_rejected: '创建被拒',
    select_player_rejected: '选角被拒'
  };

  // 新建造物模板下拉（与 onboarding::TEMPLATES 同源：blank/grazer/hunter/tower）
  var TEMPLATES = [
    { id: 'blank', label: '空白木桩（只活着）' },
    { id: 'grazer', label: '吃草彷徨者（逃跑+散步）' },
    { id: 'hunter', label: '小猎手（追击+贴身刀）' },
    { id: 'tower', label: '箭塔（站桩哨塔+盾拦）' }
  ];

  // ---- DOM ----
  var connEl = document.getElementById('wsConn');
  var tickEl = document.getElementById('wsTick');
  var warnBarEl = document.getElementById('monacoWarn');
  var listEl = document.getElementById('charList');
  var creatListEl = document.getElementById('creatList');
  var newNameEl = document.getElementById('newName');
  var newTplEl = document.getElementById('newTpl');
  var newFileEl = document.getElementById('newFile');
  var newPreviewEl = document.getElementById('newPreview');
  var newUploadEl = document.getElementById('newUpload');
  var newGenEl = document.getElementById('newGen');
  var editorWrapEl = document.getElementById('editorWrap');
  var textareaEl = document.getElementById('fallbackEditor');
  var editingNameEl = document.getElementById('editingName');
  var saveBtnEl = document.getElementById('saveBtn');
  var flashEl = document.getElementById('flash');
  var errPanelEl = document.getElementById('errPanel');
  var streamEl = document.getElementById('stream');
  var bridgeBodyEl = document.getElementById('bridgeBody');
  var bridgeListEl = document.getElementById('bridgeList');
  var delDialogEl = document.getElementById('delDialog');
  var delNameEl = document.getElementById('delName');
  var delYesEl = document.getElementById('delYes');
  var delNoEl = document.getElementById('delNo');

  // ---- 状态 ----
  var monacoReady = false;          // Monaco 可用（false=textarea 降级）
  var editor = null;                // Monaco 实例（降级时 null）
  var currentName = null;           // 正在编辑的角色名（保存目标）
  var dirty = false;                // 编辑器内容未保存标记
  var saving = false;               // 保存/删除请求在途（防双发）
  var roster = [];                  // player_roster 角色（{name,glyph,color,online}）
  var entities = [];                // 最近一帧快照实体（在线判定/造物列表数据源）
  var entById = {};                 // 最近一帧实体索引（runtime_error 归属名解析用）
  var worldTick = 0;                // 最近一帧 tick（剩余寿命计算）
  var myEntityId = null;            // 操控玩家实体 id（owner 链根；观战连接也能看到 controlled 标记）
  var eventSeq = 0;                 // 本地事件行计数（DOM key 用）
  var lastEventKey = null;          // 折叠：上一行 (entity+kind) 键
  var entityEvents = {};            // entity_id -> [事件摘要]（造物展开面板用）
  var completions = null;           // 补全数据（fetch templates/api-completions.json）
  var bridgeLoaded = false;         // 外桥配置懒加载标记
  var lastAssetId = null;           // 新建造物流程：最近上传成功的皮 asset_id
  var delTarget = null;             // 删除确认弹窗的目标名
  var delBusy = false;              // 删除弹窗决策/淡出中（防双发）
  var ws = null;
  var wsBackoff = WS_BACKOFF_INIT_MS;

  // ---- 工具 ----
  function esc(s) {
    // DOM 转义（列表/事件流全部走 textContent 或此函数，防名字注入 HTML）
    return String(s == null ? '' : s)
      .replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;')
      .replace(/"/g, '&quot;');
  }

  // 名字闸门前端镜像（与服务端 onboarding::sanitize_name 同规则，前端先验省往返）：
  // trim 后 1..=24 字符；每字符 = ASCII 字母/数字/_/- 或非 ASCII 字母（中文等）。
  // 全角数字/标点/emoji 皆拒（与后端 is_alphabetic 同锁）。
  function nameCharOk(c) {
    var code = c.charCodeAt(0);
    if ((code >= 48 && code <= 57) || (code >= 65 && code <= 90) || (code >= 97 && code <= 122)) return true;
    if (c === '_' || c === '-') return true;
    return code > 127 && /\p{L}/u.test(c);
  }
  function nameValid(raw) {
    var name = raw.trim();
    if (!name.length || name.length > 24) return false;
    for (var i = 0; i < name.length; i++) {
      if (!nameCharOk(name[i])) return false;
    }
    return true;
  }

  function flash(text, isErr) {
    flashEl.textContent = text;
    flashEl.classList.toggle('err', !!isErr);
    flashEl.hidden = false;
    clearTimeout(flash._t);
    flash._t = setTimeout(function () { flashEl.hidden = true; }, 5000);
  }

  // 角色当前是否在场：以快照实体为准（player kind 的 name；每拍都是真值，
  // roster.online 只是下发时点快照）
  function isOnline(name) {
    return entities.some(function (e) { return e.kind === 'player' && e.name === name; });
  }

  // ---- WS（观战连接：无 token。收 roster + 快照；上行不发任何意图）----
  function connect() {
    var proto = location.protocol === 'https:' ? 'wss://' : 'ws://';
    ws = new WebSocket(proto + location.host + '/ws');
    ws.onopen = function () {
      connEl.textContent = '已连接';
      connEl.className = 'ok';
      wsBackoff = WS_BACKOFF_INIT_MS;
    };
    ws.onclose = function () {
      connEl.textContent = '断线，重连中……';
      connEl.className = 'bad';
      setTimeout(connect, wsBackoff);
      wsBackoff = Math.min(wsBackoff * 2, WS_BACKOFF_MAX_MS);
    };
    ws.onerror = function () { ws.close(); };
    ws.onmessage = function (ev) {
      var msg;
      try { msg = JSON.parse(ev.data); } catch (e) { return; }
      if (msg.type === 'player_roster') {
        roster = msg.characters || [];
        renderRoster();
      } else if (msg.type === 'avatar_assets') {
        // 工坊不消费资产掩码（皮上传走 HTTP，实体渲染是战场页的事）
      } else if (msg.type === 'control_revoked') {
        // 工坊本就是无 token 观战，stale 属预期，不打扰
      } else if (msg.tick !== undefined) {
        onSnapshot(msg);
      }
    };
  }

  // ---- 快照消费（在线圆点 / 在场造物 / 事件流）----
  function onSnapshot(frame) {
    worldTick = frame.tick || 0;
    tickEl.textContent = String(worldTick);
    entities = frame.entities || [];
    entById = {};
    entities.forEach(function (e) { entById[e.id] = e; });
    var controlled = entities.filter(function (e) { return e.kind === 'player' && e.controlled; });
    myEntityId = controlled.length ? controlled[0].id : null;
    renderRoster();      // 在线圆点逐拍校真
    renderCreations();
    renderEvents(frame.player_events || []);
  }

  // ---- 角色文件列表（player_roster ∪ 本会话保存过的新角色；在线圆点逐拍校真）----
  function renderRoster() {
    var seen = {};
    var rows = [];
    roster.forEach(function (c) {
      if (!seen[c.name]) { seen[c.name] = true; rows.push(c); }
    });
    listEl.innerHTML = '';
    if (!rows.length) {
      listEl.innerHTML = '<div class="empty">还没有角色——右边新建一个，<br>或用战场页登录面板创建。</div>';
      return;
    }
    rows.forEach(function (c) {
      var online = isOnline(c.name);
      var row = document.createElement('div');
      row.className = 'crow' + (currentName === c.name ? ' sel' : '');
      row.innerHTML =
        '<span class="dot' + (online ? ' on' : '') + '" title="' + (online ? '在场' : '离场') + '"></span>' +
        '<span class="glyph" style="color:' + esc(c.color) + '">' + esc(c.glyph) + '</span>' +
        '<span class="nm">' + esc(c.name) + '</span>' +
        '<button class="del" type="button" title="删除角色文件">🗑</button>';
      row.querySelector('.nm').addEventListener('click', function () { loadCharacter(c.name); });
      row.querySelector('.del').addEventListener('click', function () { askDelete(c.name); });
      listEl.appendChild(row);
    });
  }

  // ---- 在场造物（快照 entities 里 owner=操控玩家实体 的 creation/creation_dynamic）----
  var expandedCreat = null; // 当前展开查看事件的造物实体 id
  function renderCreations() {
    creatListEl.innerHTML = '';
    var mine = entities.filter(function (e) {
      return (e.kind === 'creation' || e.kind === 'creation_dynamic') && myEntityId && e.owner === myEntityId;
    });
    if (!mine.length) {
      creatListEl.innerHTML = '<div class="empty">没有在场造物——<br>保存代码让角色进场，<br>它就能 spawn 造物。</div>';
      return;
    }
    mine.forEach(function (e) {
      var remain = e.lifespan_ticks > 0 ? e.lifespan_ticks - (worldTick - (e.born_tick || 0)) : 0;
      var row = document.createElement('div');
      row.className = 'unit' + (expandedCreat === e.id ? ' sel' : '');
      row.innerHTML =
        '<span class="glyph">' + esc(e.glyph) + '</span>' +
        '<span class="nm">' + esc(e.kind === 'creation_dynamic' ? '活物' : '造物') +
        ' <i>(' + Number(e.x).toFixed(1) + ', ' + Number(e.y).toFixed(1) + ')</i></span>' +
        '<span class="life' + (e.lifespan_ticks > 0 && remain <= 10 ? ' low' : '') + '">' +
        (e.lifespan_ticks > 0 ? remain + ' 拍' : '永生') + '</span>';
      row.addEventListener('click', function () {
        expandedCreat = expandedCreat === e.id ? null : e.id;
        renderCreations();
      });
      creatListEl.appendChild(row);
      if (expandedCreat === e.id) {
        var det = document.createElement('div');
        det.className = 'unit-detail';
        var evs = (entityEvents[e.id] || []).slice(-3).reverse();
        det.innerHTML = evs.length
          ? evs.map(function (ev) { return '<div class="uev">' + esc(ev) + '</div>'; }).join('')
          : '<div class="uev dim">最近没有事件</div>';
        creatListEl.appendChild(det);
      }
    });
  }

  // ---- 事件流（四色 + 同实体同类连续折叠 + 可点击定位）----
  function classifyEvent(ev) {
    return EVENT_CLASS[ev.kind] || 'info';
  }
  function describeEvent(ev) {
    var p = ev.payload || {};
    switch (ev.kind) {
      case 'compile_failed': return (p.name || '?') + '：' + (p.error || '');
      case 'runtime_error': return (p.which || '?') + ' 跑挂了：' + (p.error || '') + (p.died ? '（已标记死亡）' : '');
      case 'spawn_rejected': return (p.reason || '?') + (p.code_hint ? '：' + p.code_hint : '') + ' @(' + Number(p.x).toFixed(1) + ',' + Number(p.y).toFixed(1) + ')';
      case 'born': return (p.glyph || '?') + ' 出生 @(' + Number(p.x).toFixed(1) + ',' + Number(p.y).toFixed(1) + ')';
      case 'died': return '死因=' + (p.cause || '?') + (p.killer ? ' 凶手=' + String(p.killer).slice(0, 8) : '');
      case 'player_log': return 'marker=' + (p.marker !== undefined ? p.marker : '?');
      case 'player_joined': return (p.name || '') + ' 进场了';
      case 'player_selected': return (p.name || '') + ' 被选中进场';
      case 'create_player_rejected': return (p.name || '') + '：' + (p.reason || '');
      case 'select_player_rejected': return (p.name || '') + '：' + (p.reason || '');
      default: return JSON.stringify(p);
    }
  }
  function renderEvents(events) {
    events.forEach(function (ev) {
      // 首条事件到达时清掉"等待事件"占位（占位是 stream 的第一个子节点）
      var first = streamEl.firstElementChild;
      if (first && first.classList.contains('empty')) streamEl.removeChild(first);
      var cls = classifyEvent(ev);
      var desc = describeEvent(ev);
      // 造物事件缓存（展开面板显示最近 3 条）
      if (ev.entity_id) {
        var arr = entityEvents[ev.entity_id] || (entityEvents[ev.entity_id] = []);
        arr.push('t' + ev.tick + ' ' + (EVENT_LABEL[ev.kind] || ev.kind) + ' ' + desc);
        if (arr.length > ENTITY_EVENT_KEEP) arr.splice(0, arr.length - ENTITY_EVENT_KEEP);
      }
      // 同 entity+kind 连续报折叠（"又N次"，D-020a foldKey 同款）
      var key = ev.entity_id + '|' + ev.kind;
      var lastRow = streamEl.lastElementChild;
      if (lastRow && lastRow._foldKey === key && lastRow._baseDesc === desc) {
        lastRow._count++;
        lastRow.querySelector('.rep').textContent = '又 ' + lastRow._count + ' 次';
        return;
      }
      var row = document.createElement('div');
      row.className = 'evrow ' + cls;
      row._foldKey = key;
      row._baseDesc = desc;
      row._count = 1;
      var clickable = ev.kind === 'compile_failed' || ev.kind === 'spawn_rejected' || ev.kind === 'runtime_error';
      row.innerHTML =
        '<span class="tk">t' + ev.tick + '</span>' +
        '<span class="kind">' + esc(EVENT_LABEL[ev.kind] || ev.kind) + '</span>' +
        '<span class="msg">' + esc(desc) + '</span>' +
        '<span class="rep"></span>';
      if (clickable) {
        row.title = '点击定位到编辑器';
        row.addEventListener('click', function () { locateEvent(ev, desc); });
      }
      // 自动定位（卡面 R5）：编译失败/运行错与当前编辑名匹配时免点击直接标红跳转
      // ——compile_failed 带 name；runtime_error 靠快照实体 id 反查名字
      if (monacoReady && currentName) {
        if (ev.kind === 'compile_failed' && ev.payload && ev.payload.name === currentName) {
          locateEvent(ev, desc);
        } else if (ev.kind === 'runtime_error' && ev.entity_id) {
          var ent = entById[ev.entity_id];
          if (ent && ent.name === currentName) locateEvent(ev, desc);
        }
      }
      streamEl.appendChild(row);
      while (streamEl.children.length > EVENT_STREAM_CAP) streamEl.removeChild(streamEl.firstElementChild);
      streamEl.scrollTop = streamEl.scrollHeight;
    });
  }

  // ---- 错误定位链（compile_failed / spawn_rejected / runtime_error -> 编辑器）----
  function locateEvent(ev, desc) {
    if (ev.kind === 'compile_failed') {
      var errText = (ev.payload && ev.payload.error) || desc;
      showErrPanel('编译失败：' + ((ev.payload && ev.payload.name) || ''), errText);
      markCompileError(errText);
    } else if (ev.kind === 'spawn_rejected') {
      showErrPanel('造物被拒：' + ((ev.payload && ev.payload.reason) || ''), (ev.payload && ev.payload.code_hint) || '');
      markCompileError((ev.payload && ev.payload.code_hint) || '');
    } else if (ev.kind === 'runtime_error') {
      var which = (ev.payload && ev.payload.which) || '';
      showErrPanel('运行错（' + which + '）：' + ((ev.payload && ev.payload.died) ? '实体已标记死亡' : '仍在跑'), (ev.payload && ev.payload.error) || '');
      jumpToExport(which);
    }
  }

  function showErrPanel(title, body) {
    errPanelEl.innerHTML = '<b>' + esc(title) + '</b><pre>' + esc(body || '（无详情）') + '</pre>';
    errPanelEl.hidden = false;
  }

  // wat crate 错误格式实证（D-022 探针）：多行，含 `--> <anon>:行:列`。
  // 行号提取多正则兜底：先锚 --> 行，再退化 /line (\d+)/i。
  function parseErrorLine(errText) {
    var m = /-->\s*\S*?:(\d+):\d+/.exec(errText);
    if (m) return parseInt(m[1], 10);
    m = /line (\d+)/i.exec(errText);
    if (m) return parseInt(m[1], 10);
    return null;
  }

  function markCompileError(errText) {
    if (!monacoReady) return; // 降级模式只在错误面板显示
    var line = parseErrorLine(errText);
    if (line == null) return; // 提取不到行号不标红（诚实反馈，全文在错误面板）
    var model = editor.getModel();
    var maxLine = model.getLineCount();
    if (line > maxLine) return;
    monaco.editor.setModelMarkers(model, 'wat', [{
      startLineNumber: line,
      endLineNumber: line,
      startColumn: 1,
      endColumn: model.getLineMaxColumn(line),
      message: errText,
      severity: monaco.MarkerSeverity.Error
    }]);
    editor.revealLineInCenter(line);
  }

  function clearCompileMarkers() {
    if (monacoReady) monaco.editor.setModelMarkers(editor.getModel(), 'wat', []);
  }

  // 运行错定位：grep 当前文档 `(func (export "on_tick")` 所在行（尽力而为）
  function jumpToExport(funcName) {
    if (!monacoReady || !funcName) return;
    var model = editor.getModel();
    var re = new RegExp('\\(func\\s*\\(export\\s*"' + funcName.replace(/"/g, '') + '"');
    for (var ln = 1; ln <= model.getLineCount(); ln++) {
      if (re.test(model.getLineContent(ln))) {
        editor.revealLineInCenter(ln);
        editor.setPosition({ lineNumber: ln, column: 1 });
        return;
      }
    }
  }

  // ---- 编辑器（Monaco + 降级 textarea）----
  function getValue() {
    return monacoReady ? editor.getValue() : textareaEl.value;
  }
  function setValue(text) {
    if (monacoReady) {
      var model = editor.getModel();
      // 同名重载清 markers（换文件旧错误不再残留）
      monaco.editor.setModelMarkers(model, 'wat', []);
      editor.setValue(text);
    } else {
      textareaEl.value = text;
    }
    dirty = false;
    updateTitle();
  }

  function updateTitle() {
    editingNameEl.textContent = currentName || '（未选择——点左侧角色，或新建）';
    saveBtnEl.textContent = dirty ? '保存 *' : '保存';
  }

  function loadCharacter(name) {
    if (dirty && !window.confirm('「' + currentName + '」有未保存的修改，切换会丢掉。继续？')) return;
    fetch('/api/player-code?name=' + encodeURIComponent(name))
      .then(function (r) { return r.json(); })
      .then(function (j) {
        if (!j.ok) { flash('读取失败：' + (SAVE_REJECT_TEXT[j.reason] || j.reason), true); return; }
        currentName = name;
        setValue(j.code);
        errPanelEl.hidden = true;
        flash('已载入 ' + name);
      })
      .catch(function (e) { flash('读取请求失败：' + e, true); });
  }

  function saveCode() {
    if (saving) return;
    var name = (currentName || '').trim();
    if (!nameValid(name)) {
      flash('没有合法的编辑目标名（trim 后 1..=24 字符：字母/数字/中文/_/-）——先点左侧角色或在「新建造物」起名', true);
      return;
    }
    var code = getValue();
    if (new Blob([code]).size > 256 * 1024) {
      flash('代码超过 256KB 上限', true);
      return;
    }
    saving = true;
    saveBtnEl.disabled = true;
    fetch('/api/player-code', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ name: name, code: code })
    })
      .then(function (r) { return r.json(); })
      .then(function (j) {
        saving = false;
        saveBtnEl.disabled = false;
        if (j.ok) {
          dirty = false;
          updateTitle();
          mergeRosterLocal(name); // 本地合并（player_roster 只在连接时下发，不重连）
          flash(isOnline(name)
            ? '已保存 ' + name + '；该角色在场中，下次进场生效'
            : '已保存 ' + name + '（写 players/' + name + '.wat；新文件=世界下拍进场）');
        } else {
          flash('保存被拒：' + (SAVE_REJECT_TEXT[j.reason] || j.reason), true);
        }
      })
      .catch(function (e) {
        saving = false;
        saveBtnEl.disabled = false;
        flash('保存请求失败：' + e, true);
      });
  }

  // 保存成功后本地补列表项（roster 是连接时一次性下发，工坊不重连——
  // glyph=名字首字符大写、灰点离场，与世界侧规则同款；重开页面即校真）
  function mergeRosterLocal(name) {
    if (roster.some(function (c) { return c.name === name; })) { renderRoster(); return; }
    var first = name.charAt(0).toUpperCase();
    roster = roster.concat([{ name: name, glyph: first, color: '#8b949e', online: false }]);
    roster.sort(function (a, b) { return a.name < b.name ? -1 : 1; });
    renderRoster();
  }

  // ---- 删除（确认弹窗：决策即关门 0.25s 淡出 + busy 守卫，logoutPanel 同款）----
  function askDelete(name) {
    if (delBusy) return;
    delTarget = name;
    delNameEl.textContent = name;
    delDialogEl.classList.remove('closing');
    delDialogEl.hidden = false;
  }
  function closeDelDialog(after) {
    delDialogEl.classList.add('closing');
    setTimeout(function () {
      delDialogEl.hidden = true;
      delDialogEl.classList.remove('closing');
      delBusy = false;
      if (after) after();
    }, PANEL_FADE_MS);
  }
  delYesEl.addEventListener('click', function () {
    if (delBusy || !delTarget) return;
    delBusy = true;
    var name = delTarget;
    setTimeout(function () {
      fetch('/api/player-code?name=' + encodeURIComponent(name), { method: 'DELETE' })
        .then(function (r) { return r.json(); })
        .then(function (j) {
          if (!j.ok) { flash('删除被拒：' + (DELETE_REJECT_TEXT[j.reason] || j.reason), true); }
          else {
            roster = roster.filter(function (c) { return c.name !== name; });
            renderRoster();
            if (currentName === name) { currentName = null; updateTitle(); }
            flash('已删除 ' + name + '（文件没了；世界下拍残影下线）');
          }
          closeDelDialog(null);
        })
        .catch(function (e) { flash('删除请求失败：' + e, true); closeDelDialog(null); });
    }, PANEL_ACK_MS);
  });
  delNoEl.addEventListener('click', function () {
    if (delBusy) return;
    delBusy = true;
    closeDelDialog(null);
  });
  window.addEventListener('keydown', function (ev) {
    if (ev.key === 'Escape' && !delDialogEl.hidden && !delBusy) {
      delBusy = true;
      closeDelDialog(null);
    }
  });

  // ---- 新建造物（名字 + 模板 + 皮上传 -> 骨架进编辑器，不落盘）----
  TEMPLATES.forEach(function (t) {
    var opt = document.createElement('option');
    opt.value = t.id;
    opt.textContent = t.label;
    newTplEl.appendChild(opt);
  });

  newFileEl.addEventListener('change', function () {
    var f = newFileEl.files && newFileEl.files[0];
    if (!f) return;
    if (f.size > 2 * 1024 * 1024) { flash('PNG 超过 2MB 上限（上传通道护栏）', true); newFileEl.value = ''; return; }
    if (newPreviewEl.src) URL.revokeObjectURL(newPreviewEl.src);
    newPreviewEl.src = URL.createObjectURL(f);
    newPreviewEl.hidden = false;
    lastAssetId = null; // 换图后旧 asset 作废，重新上传
  });

  newUploadEl.addEventListener('click', function () {
    var f = newFileEl.files && newFileEl.files[0];
    if (!f) { flash('先选一张 PNG（透明底角色图）', true); return; }
    var name = (newNameEl.value || '').trim() || 'creation';
    var fd = new FormData();
    fd.append('file', f);
    fd.append('name', name);
    newUploadEl.disabled = true;
    fetch('/upload/avatar', { method: 'POST', body: fd })
      .then(function (r) { return r.json(); })
      .then(function (j) {
        newUploadEl.disabled = false;
        if (j.ok) {
          lastAssetId = j.asset_id;
          flash('皮已入库：' + j.asset_id + '（装戴 API 属后续刀，先记在骨架注释里）');
        } else {
          var reasonText = {
            oversize: 'PNG 超 2MB',
            oversize_dim: '原图边长超 1024px',
            decode_failed: 'PNG 解码失败',
            isolate_fail: '断件图（多个分离的实心块）',
            bad_json: '请求格式错误'
          }[j.reason] || j.reason;
          flash('上传被拒：' + reasonText, true);
        }
      })
      .catch(function (e) { newUploadEl.disabled = false; flash('上传请求失败：' + e, true); });
  });

  newGenEl.addEventListener('click', function () {
    var name = (newNameEl.value || '').trim();
    if (!nameValid(name)) {
      flash('名字不合法（trim 后 1..=24 字符：字母/数字/中文/_/-）', true);
      return;
    }
    if (dirty && !window.confirm('当前编辑器有未保存修改，生成骨架会覆盖。继续？')) return;
    var tplId = newTplEl.value;
    fetch('/templates/' + tplId + '.wat')
      .then(function (r) {
        if (!r.ok) throw new Error('模板取不到（HTTP ' + r.status + '）');
        return r.text();
      })
      .then(function (tpl) {
        var head =
          ';; —— 造物工坊骨架 ——\n' +
          ';; 名字：' + name + '　模板：' + tplId + '\n' +
          (lastAssetId
            ? ';; 皮肤资产：' + lastAssetId + '（装戴 API 属后续刀）\n'
            : ';; 皮肤资产：（未上传——传了会在这里记 asset_id；装戴 API 属后续刀）\n') +
          ';; 保存（Ctrl+S）= 写 players/' + name + '.wat，世界下拍进场\n\n';
        currentName = name;
        setValue(head + tpl);
        errPanelEl.hidden = true;
        flash('骨架已生成——改完按 Ctrl+S 保存进场');
      })
      .catch(function (e) { flash('生成失败：' + e, true); });
  });

  // ---- 外桥配置（懒加载只读展示；折叠区首次展开才拉。toggle 事件在 details 上）----
  document.getElementById('bridgeSec').addEventListener('toggle', function (ev) {
    if (!ev.target.open || bridgeLoaded) return;
    bridgeLoaded = true;
    fetch('/api/bridge-config')
      .then(function (r) { return r.json(); })
      .then(function (j) {
        var ports = (j && j.ports) || [];
        bridgeListEl.innerHTML = ports.length
          ? ports.map(function (p) {
            return '<div class="bport">端口 <b>' + esc(p.port) + '</b>' + (p.note ? ' · ' + esc(p.note) : '') + '</div>';
          }).join('')
          : '<div class="empty">没有登记的端点（桥未启用或 bridges.json 为空）</div>';
      })
      .catch(function (e) {
        bridgeListEl.innerHTML = '<div class="empty">读取失败：' + esc(String(e)) + '</div>';
      });
  });

  // ---- Monaco 装配（失败降级 textarea + 黄条）----
  function degrade(message) {
    monacoReady = false;
    editorWrapEl.hidden = true;
    textareaEl.hidden = false;
    warnBarEl.hidden = false;
    warnBarEl.textContent = '⚠ 编辑器降级为纯文本（' + message + '）——保存/删除/事件流照常，错误标红与自动补全不可用';
    document.getElementById('editorHost').classList.add('degraded');
  }

  function setupWatLanguage() {
    monaco.languages.register({ id: 'wat' });
    monaco.languages.setLanguageConfiguration('wat', {
      comments: { lineComment: ';;', blockComment: ['(;', ';)'] },
      brackets: [['(', ')']],
      autoClosingPairs: [
        { open: '(', close: ')' },
        { open: '"', close: '"', notIn: ['string', 'comment'] }
      ],
      surroundingPairs: [
        { open: '(', close: ')' },
        { open: '"', close: '"' }
      ],
      folding: {
        markers: {
          start: /^\s*\(;/,
          end: /^\s*;\)/
        }
      }
    });
    // WAT Monarch tokenizer：;; 行注释 / (; 块注释（可嵌套） / 字符串 / 十六进制与
    // 十进制数（含浮点）/ s-expr 关键字 / i32. 前缀操作码 / $ 标识符 / 括号配色
    monaco.languages.setMonarchTokensProvider('wat', {
      ignoreCase: false,
      keywords: [
        'module', 'import', 'func', 'export', 'memory', 'table', 'global',
        'param', 'result', 'type', 'local', 'data', 'elem', 'start', 'mut',
        'offset', 'item', 'if', 'then', 'else', 'block', 'loop', 'br', 'br_if',
        'br_table', 'return', 'call', 'call_indirect', 'drop', 'select', 'nop',
        'unreachable', 'end', 'ref.func', 'ref.null', 'funcref', 'externref'
      ],
      types: ['i32', 'i64', 'f32', 'f64'],
      tokenizer: {
        root: [
          [/;;.*$/, 'comment'],
          [/\(;/, 'comment', '@blockComment'],
          [/\(/, '@bracket'],
          [/\)/, '@bracket'],
          [/"(?:[^"\\]|\\.)*"/, 'string'],
          [/-?0x[0-9a-fA-F_]+/, 'number.hex'],
          [/-?\d+\.\d*(?:[eE][-+]?\d+)?/, 'number.float'],
          [/-?\d+/, 'number'],
          [/\$[a-zA-Z0-9_.<>=+\-*/\\^~?#@!&'`|]+/, 'variable'],
          [/[a-zA-Z_][\w.!<>=+\-*/\\^~?#@!&'`|]*/, {
            cases: {
              '@types': 'type',
              'i32\\..*': 'keyword',
              '@keywords': 'keyword',
              '@default': 'identifier'
            }
          }],
          [/i32\.|i64\.|f32\.|f64\./, 'keyword']
        ],
        blockComment: [
          [/;\)/, 'comment', '@pop'],
          [/\(;/, 'comment', '@push'],
          [/[^;()]+/, 'comment'],
          [/./, 'comment']
        ]
      }
    });
  }

  function setupCompletions() {
    if (!completions) return;
    // 字母触发 + 括号触发；插入文本用纯文本（snippet 语法会把 $标识符 当模板变量）
    monaco.languages.registerCompletionItemProvider('wat', {
      triggerCharacters: ['(', 'a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i', 'j', 'k', 'l', 'm', 'n', 'o', 'p', 'q', 'r', 's', 't', 'u', 'v', 'w', 'x', 'y', 'z', '_', '-'],
      provideCompletionItems: function (model, position) {
        var word = model.getWordUntilPosition(position);
        var range = {
          startLineNumber: position.lineNumber,
          endLineNumber: position.lineNumber,
          startColumn: word.startColumn,
          endColumn: word.endColumn
        };
        var typed = (word.word || '').toLowerCase();
        var suggestions = [];
        (completions.imports || []).forEach(function (imp) {
          if (typed && imp.name.toLowerCase().indexOf(typed) !== 0) return;
          suggestions.push({
            label: imp.name,
            kind: monaco.languages.CompletionItemKind.Function,
            detail: imp.signature,
            documentation: { value: '**' + imp.signature + '**\n\n' + imp.doc },
            insertText: imp.snippet,
            range: range,
            sortText: '0' + imp.name
          });
        });
        (completions.exports || []).forEach(function (ex) {
          if (typed && ex.name.toLowerCase().indexOf(typed) !== 0) return;
          suggestions.push({
            label: ex.name,
            kind: monaco.languages.CompletionItemKind.Event,
            detail: ex.signature + '（导出）',
            documentation: { value: '**' + ex.signature + '**\n\n' + ex.doc },
            insertText: ex.snippet,
            range: range,
            sortText: '1' + ex.name
          });
        });
        (completions.keywords || []).forEach(function (kw) {
          if (typed && kw.toLowerCase().indexOf(typed) !== 0) return;
          suggestions.push({
            label: kw,
            kind: monaco.languages.CompletionItemKind.Keyword,
            insertText: kw,
            range: range,
            sortText: '2' + kw
          });
        });
        return { suggestions: suggestions };
      }
    });
    // 悬浮：光标停已在文本里的 import 名上也看得到签名+中文文档
    monaco.languages.registerHoverProvider('wat', {
      provideHover: function (model, position) {
        var word = model.getWordUntilPosition(position);
        if (!word.word) return null;
        var hit = (completions.imports || []).concat((completions.exports || []).map(function (ex) {
          return { signature: ex.signature, doc: ex.doc + '（可缺省导出）' };
        })).find(function (i) { return i.name === word.word; });
        if (!hit) return null;
        var range = {
          startLineNumber: position.lineNumber,
          endLineNumber: position.lineNumber,
          startColumn: word.startColumn,
          endColumn: word.endColumn
        };
        return {
          range: range,
          contents: [
            { value: '**' + hit.signature + '**' },
            { value: hit.doc }
          ]
        };
      }
    });
  }

  function setupEditor() {
    editor = monaco.editor.create(document.getElementById('editorHost'), {
      value: ';; 点左侧角色载入代码；或右侧「新建造物」生成骨架。\n;; Ctrl+S 保存 = 写 players/<名字>.wat，世界下拍进场。\n(module\n  ;; 这里就是你在世界里的身体\n)\n',
      language: 'wat',
      theme: 'vs-dark',
      fontSize: 14,
      minimap: { enabled: false },
      automaticLayout: true,
      scrollBeyondLastLine: false,
      renderWhitespace: 'none',
      tabSize: 2
    });
    monacoReady = true;
    editor.onDidChangeModelContent(function () {
      dirty = true;
      clearCompileMarkers(); // 改动即清错误标红（改对了看得见，诚实反馈）
      updateTitle();
    });
    editor.addCommand(monaco.KeyMod.CtrlCmd | monaco.KeyCode.KeyS, saveCode);
    setupWatLanguage();
    setupCompletions();
    updateTitle();
  }

  function bootMonaco() {
    if (!window.require) {
      degrade('vendor/monaco/vs/loader.js 未加载');
      return;
    }
    require.config({ paths: { vs: 'vendor/monaco/vs' } });
    var settled = false;
    var timer = setTimeout(function () {
      if (!settled) { settled = true; degrade('加载超时'); }
    }, MONACO_TIMEOUT_MS);
    require(['vs/editor/editor.main'], function () {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      try {
        setupEditor();
      } catch (e) {
        degrade('初始化失败：' + e);
      }
    }, function (err) {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      degrade('模块加载失败：' + (err && (err.message || err.rootModuleId || '')) );
    });
  }

  // ---- 全局键位（Ctrl+S 保存；textarea 降级时同样生效）----
  window.addEventListener('keydown', function (ev) {
    if ((ev.ctrlKey || ev.metaKey) && (ev.key === 's' || ev.key === 'S')) {
      ev.preventDefault();
      saveCode();
    }
  });
  saveBtnEl.addEventListener('click', saveCode);
  textareaEl.addEventListener('input', function () {
    dirty = true;
    updateTitle();
  });

  // ---- 启动 ----
  (function boot() {
    fetch('templates/api-completions.json')
      .then(function (r) { return r.json(); })
      .then(function (j) { completions = j; })
      .catch(function () { /* 补全数据缺失不炸页面：只少补全 */ });
    bootMonaco();
    connect();
    updateTitle();
  })();
})();
