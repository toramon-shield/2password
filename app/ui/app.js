// 2password GUI（WebView 側）。暗号処理はすべて Rust 側で行い、ここでは画面だけを扱う。
// 文言は i18n.js の t()（画面の文言）と tr()（Rust が返すメッセージの訳）で出す。
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
  const el = document.createElement('div');
  el.className = 'toast' + (isErr ? ' err' : '');
  el.textContent = msg;
  $('#toasts').appendChild(el);
  setTimeout(() => el.remove(), isErr ? 7000 : 3500);
}
/** Rust から返ったエラーを、表示言語に訳して出す */
const fail = e => toast(tr(e), true);

function setBusy(form, busy, label) {
  const b = $('button[type=submit]', form);
  if (!b) return;
  if (busy) { b.dataset.label = b.textContent; b.textContent = label || t('busy.default'); }
  else if (b.dataset.label) b.textContent = b.dataset.label;
  b.disabled = busy;
}

function clearSecrets(form) {
  $$('input[type=password], input[name=secret_key]', form).forEach(i => (i.value = ''));
}

function confirmBox(title, body, yesLabel = t('btn.run')) {
  const d = $('#d-confirm');
  $('#confirm-title').textContent = title;
  $('#confirm-body').textContent = body;
  $('#confirm-yes').textContent = yesLabel;
  return new Promise(res => {
    d.addEventListener('close', () => res(d.returnValue === 'yes'), { once: true });
    d.showModal();
  });
}

const STRENGTH_COLORS = ['#dc2626', '#ea580c', '#ca8a04', '#16a34a', '#0d9488'];
async function updateMeter(input, bar, hint) {
  const v = input.value;
  if (!v) { bar.style.width = '0'; hint.textContent = t('create.hint'); return; }
  const s = await invoke('password_strength', { password: v });
  bar.style.width = ((s + 1) * 20) + '%';
  bar.style.background = STRENGTH_COLORS[s];
  hint.textContent = t('strength.label', { level: t('strength.' + s), score: s }) + (s < 3 ? ' ' + t('strength.need') : '');
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
  await I18N.init();
  $$('.lang-select').forEach(I18N.mountSelect);
  info = await invoke('app_info');
  if (!info.configured) return show('welcome');
  if (info.unlocked) return enterMain();
  $('#lock-where').textContent = info.backend === 'drive'
    ? t('lock.where_drive')
    : t('lock.where_folder', { location: info.location || '' });
  show('lock');
}

// ---------- セットアップ ----------
function mountStorage(form) {
  const slot = $('.slot-storage', form);
  slot.replaceChildren($('#t-storage').content.cloneNode(true));
  I18N.apply(slot);
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

/** Emergency Kit（印刷用の HTML）の文言。Rust 側はこれを埋め込むだけ */
function kitText() {
  return {
    lang: I18N.lang,
    dir: document.documentElement.dir,
    created: t('kit.created'),
    account_id: t('kit.account_id'),
    store: t('kit.store'),
    password: t('kit.password'),
    notes: [t('kit.warn1'), t('kit.warn2'), t('kit.warn3')],
    delete_after: t('kit.warn4'),
  };
}

const fc = $('#f-create');
fc.password.addEventListener('input', () => updateMeter(fc.password, $('#c-meter'), $('#c-hint')));
fc.addEventListener('submit', async e => {
  e.preventDefault();
  $('.error', fc).textContent = '';
  const args = storageArgs(fc);
  setBusy(fc, true, args.mode === 'drive' ? t('busy.google') : t('busy.keys'));
  try {
    const r = await invoke('create_account', { ...args, password: fc.password.value, password2: fc.password2.value, kit: kitText() });
    clearSecrets(fc);
    $('#sk-out').textContent = r.secret_key;
    $('#kit-path').textContent = t('created.kit_path', { path: r.kit_path });
    $('#b-open-kit').onclick = () => invoke('open_kit').catch(fail);
    $('#kit-done').checked = false;
    $('#b-created-next').disabled = true;
    $('#b-created-next').onclick = async () => {
      $('#sk-out').textContent = '';
      info = await invoke('app_info');
      if (info.unlocked) enterMain(); else show('lock');
    };
    show('created');
  } catch (err) {
    $('.error', fc).textContent = tr(err);
  } finally { setBusy(fc, false); }
});
$('#kit-done').addEventListener('change', e => ($('#b-created-next').disabled = !e.target.checked));

const fj = $('#f-join');
fj.addEventListener('submit', async e => {
  e.preventDefault();
  $('.error', fj).textContent = '';
  const args = storageArgs(fj);
  setBusy(fj, true, args.mode === 'drive' ? t('busy.google') : t('busy.unlocking'));
  try {
    const notices = await invoke('join_account', { ...args, secret_key: fj.secret_key.value, password: fj.password.value });
    clearSecrets(fj);
    notices.forEach(n => toast(tr(n)));
    enterMain();
  } catch (err) {
    $('.error', fj).textContent = tr(err);
  } finally { setBusy(fj, false); }
});

// ---------- ロック ----------
const fu = $('#f-unlock');
fu.addEventListener('submit', async e => {
  e.preventDefault();
  $('.error', fu).textContent = '';
  setBusy(fu, true, t('busy.unlocking'));
  try {
    const notices = await invoke('unlock', { password: fu.password.value });
    clearSecrets(fu);
    notices.forEach(n => toast(tr(n)));
    enterMain();
  } catch (err) {
    $('.error', fu).textContent = tr(err);
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
  $('#lock-where').textContent = reason === 'auto' ? t('lock.auto') : reason === 'browser' ? t('lock.browser') : '';
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
    d.textContent = items.length ? t('main.no_match') : t('main.no_items');
    list.append(d);
  }
  for (const i of shown) {
    const b = document.createElement('button');
    b.className = i.id === selected ? 'sel' : '';
    const ti = document.createElement('b'); ti.textContent = (i.has_passkey ? '🔑 ' : '') + i.title;
    const s = document.createElement('span'); s.textContent = i.username || i.urls[0] || '';
    b.append(ti, s);
    b.addEventListener('click', () => selectItem(i.id));
    list.append(b);
  }
}
$('#q').addEventListener('input', renderList);

function renderEmpty() {
  const d = document.createElement('div');
  d.className = 'empty muted';
  d.textContent = t('main.empty');
  $('#detail').replaceChildren(d);
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

async function copyField(id, which) {
  try {
    const sec = await invoke('copy_field', { id, field: which });
    toast(t('copy.done', { sec }));
  } catch (e) { fail(e); }
}

let totpTimer = null;
async function selectItem(id) {
  selected = id;
  renderList();
  let it;
  try { it = await invoke('get_item', { id }); } catch (e) { return fail(e); }
  const d = $('#detail');
  d.replaceChildren();
  const h = document.createElement('h1'); h.textContent = it.title;
  d.append(h);
  if (it.username) d.append(field(t('field.username'), span(it.username), btn(t('btn.copy'), 'secondary', () => copyField(id, 'username'))));
  const pw = span('•'.repeat(Math.min(it.password_len, 16)), 'pw');
  let shown = false;
  const reveal = btn(t('btn.show'), 'secondary', async () => {
    if (shown) { pw.textContent = '•'.repeat(Math.min(it.password_len, 16)); reveal.textContent = t('btn.show'); shown = false; return; }
    pw.textContent = await invoke('reveal_password', { id });
    reveal.textContent = t('btn.hide'); shown = true;
    setTimeout(() => { if (shown && selected === id) reveal.click(); }, 20000);
  });
  if (it.password_len) d.append(field(t('field.password'), pw, reveal, btn(t('btn.copy'), 'primary', () => copyField(id, 'password'))));
  clearInterval(totpTimer);
  if (it.has_totp) {
    const code = span('······', 'pw');
    code.dir = 'ltr';
    const left = span('', 'muted small');
    const tick = async () => {
      if (selected !== id || $('#v-main').hidden) return clearInterval(totpTimer);
      try {
        const c = await invoke('totp_code', { id });
        code.textContent = c.code.replace(/(.{3})(?=.)/g, '$1 ');
        left.textContent = t('detail.totp_left', { sec: c.remaining });
      } catch (e) { clearInterval(totpTimer); left.textContent = tr(e); }
    };
    tick();
    totpTimer = setInterval(tick, 1000);
    d.append(field(t('field.totp'), code, left,
      btn(t('btn.copy'), 'primary', () => copyField(id, 'totp')),
      btn(t('btn.remove'), 'secondary', async () => {
        if (!await confirmBox(t('totp.remove_title'), t('totp.remove_body', { title: it.title }), t('btn.remove'))) return;
        try { await invoke('set_totp', { id, value: '' }); toast(t('totp.removed')); selectItem(id); }
        catch (e) { fail(e); }
      })));
  }
  if (it.passkey) {
    const who = it.passkey.user_name || t('detail.no_username');
    d.append(field(t('field.passkey'), span(t('detail.passkey', { rp: it.passkey.rp_id, user: who, created: it.passkey.created })),
      btn(t('btn.remove'), 'secondary', async () => {
        if (!await confirmBox(t('passkey.remove_title'), t('passkey.remove_body', { title: it.title }), t('btn.remove'))) return;
        try { await invoke('remove_passkey', { id }); toast(t('passkey.removed')); await refreshList(); selectItem(id); }
        catch (e) { fail(e); }
      })));
  }
  for (const u of it.urls) {
    const a = document.createElement('a'); a.textContent = u; a.dir = 'ltr';
    a.addEventListener('click', () => invoke('open_url', { url: u }).catch(fail));
    d.append(field(t('field.url'), a));
  }
  if (it.notes) d.append(field(t('field.notes'), span(it.notes, 'notes')));
  const meta = document.createElement('p'); meta.className = 'muted small'; meta.textContent = t('detail.updated', { time: it.updated });
  const actions = document.createElement('div'); actions.className = 'actions';
  actions.append(
    btn(t('btn.delete'), 'danger', async () => {
      if (!await confirmBox(t('delete.title'), t('delete.body', { title: it.title }), t('btn.delete'))) return;
      try { await invoke('delete_item', { id }); selected = null; toast(t('delete.done')); refreshList(); }
      catch (e) { fail(e); }
    }),
    btn(t('btn.edit'), 'secondary', () => openEditor(it)),
  );
  d.append(meta, actions);
}

// ---------- 追加・編集 ----------
let editingId = null;
const fe = $('#f-edit');
function openEditor(it) {
  editingId = it ? it.id : null;
  $('#edit-title').textContent = it ? t('edit.edit') : t('edit.add');
  fe.reset();
  $('.error', fe).textContent = '';
  fe.password.type = 'password';
  fe.password.placeholder = it ? t('edit.password_keep') : t('edit.password_new');
  fe.totp.placeholder = it && it.has_totp ? t('edit.totp_set') : t('edit.totp_empty');
  if (it) {
    fe.title.value = it.title; fe.username.value = it.username;
    fe.urls.value = it.urls.join('\n'); fe.notes.value = it.notes;
  }
  $('#d-edit').showModal();
  fe.title.focus();
}
$('#b-add').addEventListener('click', () => openEditor(null));
$('#b-edit-cancel').addEventListener('click', () => { fe.password.value = ''; fe.totp.value = ''; $('#d-edit').close(); });
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
  save.disabled = true; save.textContent = t('busy.saving');
  try {
    const id = await invoke('save_item', { id: editingId, form });
    fe.password.value = '';
    const totp = fe.totp.value.trim();
    fe.totp.value = '';
    if (totp) {
      try { await invoke('set_totp', { id, value: totp }); }
      catch (err) { toast(t('edit.totp_failed', { error: tr(err) }), true); }
    }
    $('#d-edit').close();
    toast(t('edit.saved'));
    selected = id;
    await refreshList();
  } catch (err) {
    $('.error', fe).textContent = tr(err);
  } finally { save.disabled = false; save.textContent = t('btn.save'); }
});

// ---------- 同期 ----------
$('#b-sync').addEventListener('click', async () => {
  const b = $('#b-sync'); b.disabled = true;
  try { (await invoke('sync_now')).forEach(n => toast(tr(n))); await refreshList(); }
  catch (e) { fail(e); }
  finally { b.disabled = false; }
});

// ---------- 設定 ----------
async function openSettings() {
  const d = $('#d-settings');
  d.showModal();
  const kv = $('#st-kv');
  const loading = document.createElement('dt');
  loading.textContent = t('settings.loading');
  kv.replaceChildren(loading);
  $('#st-hex').textContent = '';
  try {
    const s = await invoke('status');
    kv.replaceChildren();
    const rows = [
      [t('settings.backend'), s.backend === 'drive' ? t('settings.backend_drive') : t('storage.folder')],
      [t('settings.storage'), s.location],
      [t('settings.aid'), s.aid],
      [t('settings.counter'), String(s.known_counter)],
      ...s.files.map(([n, size]) => [n, size == null ? t('settings.file_missing') : t('settings.bytes', { n: size })]),
      [t('settings.home'), s.home],
    ];
    for (const [k, v] of rows) {
      const dt = document.createElement('dt'); dt.textContent = k;
      const dd = document.createElement('dd'); dd.textContent = v;
      kv.append(dt, dd);
    }
    $('#st-hex').textContent = s.sample_hex ? t('settings.sample', { hex: s.sample_hex }) : '';
    $('#b-migrate').hidden = !(s.backend === 'folder' && s.oauth_ready);
    $('#b-google-login').disabled = !s.oauth_ready;
  } catch (e) { kv.replaceChildren(); fail(e); }
  $('#st-version').textContent = '2password ' + info.version;
  refreshBrowser();
}
$('#b-settings').addEventListener('click', openSettings);
$('#b-settings-close').addEventListener('click', () => $('#d-settings').close());

$('#b-google-login').addEventListener('click', async () => {
  toast(t('google.login_prompt'));
  try { await invoke('google_login'); toast(t('google.logged_in')); } catch (e) { fail(e); }
});
$('#b-google-logout').addEventListener('click', async () => {
  if (!await confirmBox(t('google.logout_title'), t('google.logout_body'), t('google.logout_yes'))) return;
  try { await invoke('google_logout'); toast(t('google.logged_out')); } catch (e) { fail(e); }
});
$('#b-migrate').addEventListener('click', async () => {
  if (!await confirmBox(t('migrate.title'), t('migrate.body'), t('migrate.yes'))) return;
  try { const loc = await invoke('migrate_to_drive'); toast(t('migrate.done', { location: loc })); openSettings(); } catch (e) { fail(e); }
});

const fp = $('#f-passwd');
fp.addEventListener('submit', async e => {
  e.preventDefault();
  try {
    await invoke('change_password', { current: fp.current.value, password: fp.password.value, password2: fp.password2.value });
    toast(t('settings.passwd_done'));
  } catch (err) { fail(err); }
  finally { clearSecrets(fp); }
});

$('#b-forget').addEventListener('click', async () => {
  if (!await confirmBox(t('settings.device_forget_title'), t('settings.device_forget_body'), t('settings.device_forget_yes'))) return;
  try { await invoke('forget_device'); $('#d-settings').close(); info = await invoke('app_info'); show('welcome'); toast(t('settings.device_forgot')); }
  catch (e) { fail(e); }
});

// ---------- ブラウザ拡張機能 ----------
async function refreshBrowser() {
  const b = await invoke('browser_status');
  $('#br-state').textContent = b.installed ? t('settings.browser_connected') : t('settings.browser_not_connected');
  $('#b-browser-uninstall').hidden = !b.installed;
  $('#br-ids').textContent = t('settings.browser_ids', {
    ids: I18N.list([t('settings.browser_dev', { id: b.dev_extension_id }), ...b.extra_extension_ids]),
  });
  $('#br-approved').textContent = t('settings.browser_approved', { n: b.approved_sites });
  const log = await invoke('browser_log');
  $('#br-log').replaceChildren(...(log.length ? log : [null]).map(e => {
    const li = document.createElement('li');
    li.textContent = e
      ? [new Date(e.at * 1000).toLocaleTimeString(I18N.lang), t('log.' + e.event), e.host, e.title].filter(Boolean).join('　')
      : t('settings.browser_no_log');
    return li;
  }));
}
$('#b-browser-forget').addEventListener('click', async () => {
  if (!await confirmBox(t('settings.forget_title'), t('settings.forget_body'), t('settings.forget_yes'))) return;
  try { await invoke('browser_forget_approvals'); toast(t('settings.forget_done')); } catch (e) { fail(e); }
  refreshBrowser();
});

// 拡張機能が、項目をそのサイトで初めて使おうとしたときの確認
let approving = null; // 画面に出している確認
async function askApproval() {
  const d = $('#d-approve');
  const p = await invoke('browser_pending');
  if (!p || $('#v-main').hidden) return;
  approving = p;
  $('#ap-what').textContent = p.passkey ? t('approve.passkey') : t('approve.password');
  $('#ap-host').textContent = p.host;
  $('#ap-title').textContent = p.title;
  $('#ap-user').textContent = p.username || t('approve.none');
  if (!d.open) d.showModal();
}
$('#d-approve').addEventListener('close', async e => {
  const allow = e.target.returnValue === 'yes';
  e.target.returnValue = '';
  const p = approving;
  approving = null;
  if (!p) return;
  try {
    await invoke('browser_approve', { allow, itemId: p.item_id, origin: p.origin });
    if (allow) toast(t('approve.allowed'));
  } catch (err) { if (allow) fail(err); }
});
listen('browser-approval', askApproval);
$('#b-browser-install').addEventListener('click', async () => {
  try {
    const browsers = await invoke('browser_install', { extensionId: $('#br-extid').value.trim() || null });
    $('#br-extid').value = '';
    toast(t('settings.browser_connected_toast', { browsers: I18N.list(browsers) }));
  } catch (e) { fail(e); }
  refreshBrowser();
});
$('#b-browser-uninstall').addEventListener('click', async () => {
  try { await invoke('browser_uninstall'); toast(t('settings.browser_disconnected_toast')); } catch (e) { fail(e); }
  refreshBrowser();
});
// 拡張機能から保存・更新されたら一覧を更新する
listen('vault-changed', () => { if (!$('#v-main').hidden) refreshList(); });

// ---------- CSV の取り込み ----------
$('#b-import').addEventListener('click', () => $('#f-import').click());
$('#f-import').addEventListener('change', async e => {
  const file = e.target.files[0];
  e.target.value = '';
  if (!file) return;
  if (file.size > 20 * 1024 * 1024) return toast(t('settings.import_too_big'), true);
  try {
    const [added, skipped] = await invoke('import_csv', { text: await file.text() });
    toast(t('settings.import_done', { added, skipped }));
    await refreshList();
  } catch (err) { fail(err); }
});

boot().catch(e => { document.body.textContent = (typeof t === 'function' ? t('boot.failed', { error: tr(e) }) : 'Failed to start: ' + e); });
