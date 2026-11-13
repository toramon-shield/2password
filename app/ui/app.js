// 2password GUI（WebView 側）。暗号処理はすべて Rust 側で行い、ここでは画面だけを扱う。
'use strict';

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const $ = (s, el = document) => el.querySelector(s);
const $$ = (s, el = document) => [...el.querySelectorAll(s)];

// ---------- 共通 ----------
function show(view) {
  $$('.view').forEach(v => (v.hidden = v.id !== 'v-' + view));
  const first = $('#v-' + view + ' input:not([type=radio]):not([hidden])');
  if (first) setTimeout(() => first.focus(), 30);
}

function toast(msg, isErr = false) {
  const t = document.createElement('div');
  t.className = 'toast' + (isErr ? ' err' : '');
  t.textContent = msg;
  $('#toasts').appendChild(t);
  setTimeout(() => t.remove(), isErr ? 7000 : 3500);
}

function setBusy(form, busy, label) {
  const b = $('button[type=submit]', form);
  if (!b) return;
  if (busy) { b.dataset.label = b.textContent; b.textContent = label || '処理中…'; }
  else if (b.dataset.label) b.textContent = b.dataset.label;
  b.disabled = busy;
}

function clearSecrets(form) {
  $$('input[type=password], input[name=secret_key]', form).forEach(i => (i.value = ''));
}

function confirmBox(title, body, yesLabel = '実行') {
  const d = $('#d-confirm');
  $('#confirm-title').textContent = title;
  $('#confirm-body').textContent = body;
  $('#confirm-yes').textContent = yesLabel;
  return new Promise(res => {
    d.addEventListener('close', () => res(d.returnValue === 'yes'), { once: true });
    d.showModal();
  });
}

const STRENGTH = [
  ['#dc2626', 'とても弱い'], ['#ea580c', '弱い'], ['#ca8a04', 'もう少し'],
  ['#16a34a', '強い（使えます）'], ['#0d9488', 'とても強い'],
];
async function updateMeter(input, bar, hint) {
  const v = input.value;
  if (!v) { bar.style.width = '0'; hint.textContent = '単語を4〜5個つなげた、長いパスフレーズがおすすめです。'; return; }
  const s = await invoke('password_strength', { password: v });
  bar.style.width = ((s + 1) * 20) + '%';
  bar.style.background = STRENGTH[s][0];
  hint.textContent = `強度: ${STRENGTH[s][1]}（${s}/4）` + (s < 3 ? '　3 以上が必要です' : '');
}

// 操作があったことを Rust 側に伝える（自動ロックの計測用。最大 15 秒に1回）
let lastTouch = 0;
['pointerdown', 'keydown'].forEach(ev => document.addEventListener(ev, () => {
  const now = Date.now();
  if (now - lastTouch > 15000 && !$('#v-main').hidden) { lastTouch = now; invoke('touch'); }
}, { passive: true }));

// ---------- 起動 ----------
let info = null;
async function boot() {
  info = await invoke('app_info');
  if (!info.configured) return show('welcome');
  if (info.unlocked) return enterMain();
  $('#lock-where').textContent = info.backend === 'drive' ? '保存先: Google Drive' : '保存先: ' + (info.location || '');
  show('lock');
}

// ---------- セットアップ ----------
function mountStorage(form) {
  const slot = $('.slot-storage', form);
  slot.replaceChildren($('#t-storage').content.cloneNode(true));
  const path = $('.folder-path', form);
  if (info.detected_drive_folder) path.value = info.detected_drive_folder;
  const sync = () => {
    const mode = $('input[name=mode]:checked', form).value;
    path.hidden = mode !== 'folder';
    $('.oauth-missing', form).hidden = !(mode === 'drive' && !info.oauth_ready);
  };
  $$('input[name=mode]', form).forEach(r => r.addEventListener('change', sync));
  if (!info.oauth_ready) $('input[value=folder]', form).checked = true;
  sync();
}
function storageArgs(form) {
  const mode = $('input[name=mode]:checked', form).value;
  return { mode, folder: mode === 'folder' ? $('.folder-path', form).value : null };
}

$$('[data-go]').forEach(b => b.addEventListener('click', () => {
  const to = b.dataset.go;
  if (to === 'create') mountStorage($('#f-create'));
  if (to === 'join') mountStorage($('#f-join'));
  show(to);
}));

const fc = $('#f-create');
fc.password.addEventListener('input', () => updateMeter(fc.password, $('#c-meter'), $('#c-hint')));
fc.addEventListener('submit', async e => {
  e.preventDefault();
  $('.error', fc).textContent = '';
  const args = storageArgs(fc);
  setBusy(fc, true, args.mode === 'drive' ? 'ブラウザで Google にログインしてください…' : '鍵を作っています…');
  try {
    const r = await invoke('create_account', { ...args, password: fc.password.value, password2: fc.password2.value });
    clearSecrets(fc);
    $('#sk-out').textContent = r.secret_key;
    $('#kit-path').textContent = 'Emergency Kit: ' + r.kit_path;
    $('#b-open-kit').onclick = () => invoke('open_kit').catch(e => toast(String(e), true));
    $('#kit-done').checked = false;
    $('#b-created-next').disabled = true;
    $('#b-created-next').onclick = async () => {
      $('#sk-out').textContent = '';
      info = await invoke('app_info');
      if (info.unlocked) enterMain(); else show('lock');
    };
    show('created');
  } catch (err) {
    $('.error', fc).textContent = err;
  } finally { setBusy(fc, false); }
});
$('#kit-done').addEventListener('change', e => ($('#b-created-next').disabled = !e.target.checked));

const fj = $('#f-join');
fj.addEventListener('submit', async e => {
  e.preventDefault();
  $('.error', fj).textContent = '';
  const args = storageArgs(fj);
  setBusy(fj, true, args.mode === 'drive' ? 'ブラウザで Google にログインしてください…' : '解除しています…');
  try {
    const notices = await invoke('join_account', { ...args, secret_key: fj.secret_key.value, password: fj.password.value });
    clearSecrets(fj);
    notices.forEach(n => toast(n));
    enterMain();
  } catch (err) {
    $('.error', fj).textContent = err;
  } finally { setBusy(fj, false); }
});

// ---------- ロック ----------
const fu = $('#f-unlock');
fu.addEventListener('submit', async e => {
  e.preventDefault();
  $('.error', fu).textContent = '';
  setBusy(fu, true, '解除しています…');
  try {
    const notices = await invoke('unlock', { password: fu.password.value });
    clearSecrets(fu);
    notices.forEach(n => toast(n));
    enterMain();
  } catch (err) {
    $('.error', fu).textContent = err;
    fu.password.select();
  } finally { setBusy(fu, false); }
});

async function lockNow(reason) {
  await invoke('lock');
  // 画面に残った平文（一覧・詳細・入力途中のフォーム・Secret Key 表示）を消す
  items = []; selected = null; editingId = null;
  $('#detail').replaceChildren();
  $('#list').replaceChildren();
  $('#q').value = '';
  $('#sk-out').textContent = '';
  $$('dialog[open]').forEach(d => d.close());
  $$('form').forEach(f => f.reset());
  $$('input[type=text][name=password]').forEach(i => (i.type = 'password'));
  info = await invoke('app_info');
  $('#lock-where').textContent = reason === 'auto' ? '5分間操作がなかったため、自動でロックしました' : '';
  show('lock');
}
$('#b-lock').addEventListener('click', () => lockNow('manual'));
listen('locked', ev => lockNow(ev.payload));

// ---------- 一覧と詳細 ----------
let items = [];
let selected = null;

async function enterMain() {
  show('main');
  await refreshList();
}

async function refreshList() {
  items = await invoke('list_items');
  renderList();
  if (selected && items.some(i => i.id === selected)) selectItem(selected);
  else { selected = null; renderEmpty(); }
}

function renderList() {
  const q = $('#q').value.trim().toLowerCase();
  const list = $('#list');
  list.replaceChildren();
  const shown = items.filter(i => !q || [i.title, i.username, ...i.urls].some(s => s.toLowerCase().includes(q)));
  if (!shown.length) {
    const d = document.createElement('div');
    d.className = 'none';
    d.textContent = items.length ? '一致する項目がありません' : 'まだ項目がありません';
    list.append(d);
  }
  for (const i of shown) {
    const b = document.createElement('button');
    b.className = i.id === selected ? 'sel' : '';
    const t = document.createElement('b'); t.textContent = i.title;
    const s = document.createElement('span'); s.textContent = i.username || i.urls[0] || '';
    b.append(t, s);
    b.addEventListener('click', () => selectItem(i.id));
    list.append(b);
  }
}
$('#q').addEventListener('input', renderList);

function renderEmpty() {
  $('#detail').innerHTML = '<div class="empty muted">左の一覧から項目を選ぶか、「＋ 追加」で登録してください。</div>';
}

function field(label, valueNode, ...buttons) {
  const f = document.createElement('div'); f.className = 'field';
  const l = document.createElement('div'); l.className = 'lbl'; l.textContent = label;
  const v = document.createElement('div'); v.className = 'val';
  valueNode.classList.add('text');
  v.append(valueNode, ...buttons);
  f.append(l, v);
  return f;
}
function btn(label, cls, fn) {
  const b = document.createElement('button');
  b.className = cls; b.textContent = label; b.type = 'button';
  b.addEventListener('click', fn);
  return b;
}
function span(text, cls = '') {
  const s = document.createElement('span'); s.textContent = text; if (cls) s.className = cls; return s;
}

async function copyField(id, field) {
  try {
    const secs = await invoke('copy_field', { id, field });
    toast(`コピーしました（${secs} 秒後にクリップボードから消えます）`);
  } catch (e) { toast(String(e), true); }
}

async function selectItem(id) {
  selected = id;
  renderList();
  let it;
  try { it = await invoke('get_item', { id }); } catch (e) { return toast(String(e), true); }
  const d = $('#detail');
  d.replaceChildren();
  const h = document.createElement('h1'); h.textContent = it.title;
  d.append(h);
  if (it.username) d.append(field('ユーザー名', span(it.username), btn('コピー', 'secondary', () => copyField(id, 'username'))));
  const pw = span('•'.repeat(Math.min(it.password_len, 16)), 'pw');
  let shown = false;
  const reveal = btn('表示', 'secondary', async () => {
    if (shown) { pw.textContent = '•'.repeat(Math.min(it.password_len, 16)); reveal.textContent = '表示'; shown = false; return; }
    pw.textContent = await invoke('reveal_password', { id });
    reveal.textContent = '隠す'; shown = true;
    setTimeout(() => { if (shown && selected === id) reveal.click(); }, 20000);
  });
  d.append(field('パスワード', pw, reveal, btn('コピー', 'primary', () => copyField(id, 'password'))));
  for (const u of it.urls) {
    const a = document.createElement('a'); a.textContent = u;
    a.addEventListener('click', () => invoke('open_url', { url: u }).catch(e => toast(String(e), true)));
    d.append(field('URL', a));
  }
  if (it.notes) d.append(field('メモ', span(it.notes, 'notes')));
  const meta = document.createElement('p'); meta.className = 'muted small'; meta.textContent = '更新: ' + it.updated;
  const actions = document.createElement('div'); actions.className = 'actions';
  actions.append(
    btn('削除', 'danger', async () => {
      if (!await confirmBox('削除しますか？', `「${it.title}」を削除します。他の端末にも反映されます。`, '削除')) return;
      try { await invoke('delete_item', { id }); selected = null; toast('削除しました'); refreshList(); }
      catch (e) { toast(String(e), true); }
    }),
    btn('編集', 'secondary', () => openEditor(it)),
  );
  d.append(meta, actions);
}

// ---------- 追加・編集 ----------
let editingId = null;
const fe = $('#f-edit');
function openEditor(it) {
  editingId = it ? it.id : null;
  $('#edit-title').textContent = it ? '項目を編集' : '項目を追加';
  fe.reset();
  $('.error', fe).textContent = '';
  fe.password.type = 'password';
  fe.password.placeholder = it ? '空なら変更しない' : '空なら自動生成';
  if (it) {
    fe.title.value = it.title; fe.username.value = it.username;
    fe.urls.value = it.urls.join('\n'); fe.notes.value = it.notes;
  }
  $('#d-edit').showModal();
  fe.title.focus();
}
$('#b-add').addEventListener('click', () => openEditor(null));
$('#b-edit-cancel').addEventListener('click', () => { fe.password.value = ''; $('#d-edit').close(); });
$('#b-gen').addEventListener('click', async () => {
  fe.password.value = await invoke('generate_password', { length: 24 });
  fe.password.type = 'text';
});
$('#b-peek').addEventListener('click', () => { fe.password.type = fe.password.type === 'password' ? 'text' : 'password'; });
fe.addEventListener('submit', async e => {
  e.preventDefault();
  const form = {
    title: fe.title.value, username: fe.username.value,
    urls: fe.urls.value.split('\n'), password: fe.password.value, notes: fe.notes.value,
  };
  const save = $('#b-edit-save');
  save.disabled = true; save.textContent = '保存中…';
  try {
    const id = await invoke('save_item', { id: editingId, form });
    fe.password.value = '';
    $('#d-edit').close();
    toast('暗号化して保存しました');
    selected = id;
    await refreshList();
  } catch (err) {
    $('.error', fe).textContent = err;
  } finally { save.disabled = false; save.textContent = '保存'; }
});

// ---------- 同期 ----------
$('#b-sync').addEventListener('click', async () => {
  const b = $('#b-sync'); b.disabled = true;
  try { (await invoke('sync_now')).forEach(n => toast(n)); await refreshList(); }
  catch (e) { toast(String(e), true); }
  finally { b.disabled = false; }
});

// ---------- 設定 ----------
async function openSettings() {
  const d = $('#d-settings');
  d.showModal();
  const kv = $('#st-kv'); kv.innerHTML = '<dt>読み込み中…</dt><dd></dd>';
  $('#st-hex').textContent = '';
  try {
    const s = await invoke('status');
    kv.replaceChildren();
    const rows = [
      ['保管方式', s.backend === 'drive' ? 'Google Drive API（drive.file）' : '同期フォルダ'],
      ['保管場所', s.location],
      ['アカウントID', s.aid],
      ['保存番号', String(s.known_counter)],
      ...s.files.map(([n, size]) => [n, size == null ? '（なし）' : size + ' バイト']),
      ['この端末の設定', s.home],
    ];
    for (const [k, v] of rows) {
      const dt = document.createElement('dt'); dt.textContent = k;
      const dd = document.createElement('dd'); dd.textContent = v;
      kv.append(dt, dd);
    }
    $('#st-hex').textContent = s.sample_hex ? '暗号文の例: ' + s.sample_hex + '…' : '';
    $('#b-migrate').hidden = !(s.backend === 'folder' && s.oauth_ready);
    $('#b-google-login').disabled = !s.oauth_ready;
  } catch (e) { kv.innerHTML = ''; toast(String(e), true); }
  $('#st-version').textContent = '2password ' + info.version;
}
$('#b-settings').addEventListener('click', openSettings);
$('#b-settings-close').addEventListener('click', () => $('#d-settings').close());

$('#b-google-login').addEventListener('click', async () => {
  toast('ブラウザで Google にログインしてください');
  try { await invoke('google_login'); toast('Google にログインしました'); } catch (e) { toast(String(e), true); }
});
$('#b-google-logout').addEventListener('click', async () => {
  if (!await confirmBox('Google からログアウト', 'この端末の Google トークンを無効化します。次に同期するときは、もう一度ログインが必要です。', 'ログアウト')) return;
  try { await invoke('google_logout'); toast('ログアウトしました'); } catch (e) { toast(String(e), true); }
});
$('#b-migrate').addEventListener('click', async () => {
  if (!await confirmBox('Google Drive へ移す', '保管庫を暗号文のまま Google Drive（2password-vault フォルダ）へ移します。パスワードと Secret Key は変わりません。', '移す')) return;
  try { const loc = await invoke('migrate_to_drive'); toast('移しました: ' + loc); openSettings(); } catch (e) { toast(String(e), true); }
});

const fp = $('#f-passwd');
fp.addEventListener('submit', async e => {
  e.preventDefault();
  try {
    await invoke('change_password', { current: fp.current.value, password: fp.password.value, password2: fp.password2.value });
    toast('アカウントパスワードを変更しました');
  } catch (err) { toast(String(err), true); }
  finally { clearSecrets(fp); }
});

$('#b-forget').addEventListener('click', async () => {
  if (!await confirmBox('この端末から消しますか？', 'この端末の設定と Secret Key を消します。保管場所の暗号文は残ります。再び使うには Emergency Kit の Secret Key が必要です。', '消す')) return;
  try { await invoke('forget_device'); $('#d-settings').close(); info = await invoke('app_info'); show('welcome'); toast('この端末から消しました'); }
  catch (e) { toast(String(e), true); }
});

boot().catch(e => { document.body.textContent = '起動に失敗しました: ' + e; });
