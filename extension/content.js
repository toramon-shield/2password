// 2password ブラウザ拡張：ページの中で動く部分
//
// - ログイン欄にフォーカスが当たったら、このサイトに一致する項目を候補として出す
// - 利用者が候補をクリックしたときだけ、ユーザー名とパスワードを入力する（自動では入力しない）
// - ログインや登録の送信を見つけたら、次の画面で「保存しますか？」と確認する
// - 項目を選んでログインしたあとに認証コードの欄が出たら、ワンタイムパスワードも入力する
// - サイトがパスキーを作る・使うときは、2password を保管場所として選べるようにする
// - 表示する部品は closed な Shadow DOM の中に作り、ページのスクリプトから読めないようにする
(() => {
  'use strict';
  if (window.top !== window) return; // 最上位のフレームだけ

  const send = msg => chrome.runtime.sendMessage(msg).catch(() => ({ ok: false, error: 'no_background' }));
  // 文言は _locales/<言語>/messages.json にある（ブラウザの表示言語で選ばれる）
  const t = (key, ...subs) => chrome.i18n.getMessage(key, subs.map(String)) || key;
  /** アプリから返ったエラーを、コードから訳す（アプリが返す文は日本語なので使わない） */
  const errText = res => chrome.i18n.getMessage('err_' + (res && res.error)) || t('err_failed');
  const DIR = chrome.i18n.getMessage('@@bidi_dir') || 'ltr';
  const LANG = chrome.i18n.getUILanguage();

  // ---------------- 入力欄の判定 ----------------

  const visible = el => {
    if (!el.isConnected || el.disabled || el.readOnly) return false;
    const r = el.getBoundingClientRect();
    if (r.width < 8 || r.height < 8) return false;
    const s = getComputedStyle(el);
    return s.visibility !== 'hidden' && s.display !== 'none' && s.opacity !== '0';
  };
  const isPassword = el => el instanceof HTMLInputElement && el.type === 'password';
  const isTextual = el => el instanceof HTMLInputElement && ['text', 'email', 'tel', ''].includes(el.type || '');
  const scopeOf = el => el.form || document;
  const inputsIn = scope => [...scope.querySelectorAll('input')].filter(visible);

  /** ユーザー名らしい欄か（パスワード欄のないページ＝複数ページのログイン用） */
  function looksLikeUsername(el) {
    if (!isTextual(el)) return false;
    const ac = (el.autocomplete || '').toLowerCase();
    if (ac.includes('username') || ac === 'email') return true;
    const hint = `${el.name} ${el.id}`.toLowerCase();
    return el.type === 'email' || /user|login|email|account|mail|userid|loginid/.test(hint);
  }

  /** パスワード欄の手前にある、ユーザー名の欄 */
  function usernameFieldFor(pw) {
    const all = inputsIn(scopeOf(pw));
    const before = all.slice(0, all.indexOf(pw)).filter(isTextual);
    return before.reverse().find(looksLikeUsername) || before[0] || null;
  }

  /** フォーカスされた欄から、入力先（ユーザー名欄・パスワード欄）を決める */
  function targetsFor(el) {
    if (isPassword(el)) return { user: usernameFieldFor(el), pass: el };
    if (!isTextual(el)) return null;
    const all = inputsIn(scopeOf(el));
    const pass = all.slice(all.indexOf(el) + 1).find(isPassword);
    if (pass && usernameFieldFor(pass) === el) return { user: el, pass };
    if (!pass && looksLikeUsername(el) && !all.some(isPassword)) return { user: el, pass: null };
    return null;
  }

  /** React などの画面でも値が反映されるように入力する */
  function setValue(el, value) {
    const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set;
    el.focus({ preventScroll: true });
    setter.call(el, value);
    el.dispatchEvent(new Event('input', { bubbles: true }));
    el.dispatchEvent(new Event('change', { bubbles: true }));
  }

  function fill(targets, username, password) {
    if (targets.user && username && visible(targets.user)) setValue(targets.user, username);
    if (targets.pass && visible(targets.pass)) setValue(targets.pass, password);
    return Boolean(targets.pass || targets.user);
  }

  // ---------------- 表示（closed な Shadow DOM） ----------------

  const CSS = `
    :host { all: initial; }
    .box { position: fixed; z-index: 2147483647; box-sizing: border-box; font: 14px/1.4 system-ui, -apple-system, "Segoe UI", "Hiragino Sans", "Yu Gothic UI", sans-serif;
           color: #111827; background: #fff; border: 1px solid #d1d5db; border-radius: 10px; box-shadow: 0 8px 28px rgba(0,0,0,.18); overflow: hidden; }
    .head { display: flex; align-items: center; gap: 8px; padding: 8px 12px; font-size: 12px; color: #6b7280; border-bottom: 1px solid #e5e7eb; }
    .logo { display: inline-grid; place-items: center; width: 20px; height: 20px; border-radius: 6px; background: #4f46e5; color: #fff; font-weight: 700; font-size: 10px; }
    button { all: unset; box-sizing: border-box; cursor: pointer; font: inherit; }
    .item { display: block; width: 100%; padding: 8px 12px; }
    .item:hover, .item:focus-visible { background: #eef2ff; }
    .item b { display: block; font-weight: 600; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
    .item span { display: block; font-size: 12px; color: #6b7280; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
    .banner { top: 16px; inset-inline-end: 16px; width: 320px; padding: 14px; }
    .banner h1 { margin: 0 0 6px; font-size: 14px; font-weight: 700; display: flex; align-items: center; gap: 8px; }
    .banner p { margin: 0 0 12px; font-size: 13px; color: #374151; word-break: break-all; }
    .row { display: flex; gap: 8px; justify-content: flex-end; }
    .btn { padding: 7px 14px; border-radius: 8px; font-size: 13px; font-weight: 600; background: #f3f4f6; color: #111827; }
    .btn.primary { background: #4f46e5; color: #fff; }
    .btn:hover { filter: brightness(.96); }
    .note { padding: 8px 12px; font-size: 12px; color: #374151; }
    .note b { display: block; font-size: 13px; color: #111827; }
    .note.warn { background: #fef2f2; color: #991b1b; }
    .note.warn b { color: #991b1b; }
    @media (prefers-color-scheme: dark) {
      .box { color: #f3f4f6; background: #1f2937; border-color: #374151; }
      .head { color: #9ca3af; border-color: #374151; }
      .item:hover, .item:focus-visible { background: #312e81; }
      .item span { color: #9ca3af; }
      .banner p { color: #d1d5db; }
      .btn { background: #374151; color: #f3f4f6; }
      .btn.primary { background: #6366f1; color: #fff; }
      .note { color: #d1d5db; }
      .note b { color: #f3f4f6; }
      .note.warn { background: #7f1d1d; color: #fecaca; }
      .note.warn b { color: #fff; }
    }`;

  function makeLayer() {
    const host = document.createElement('div');
    host.lang = LANG;
    host.dir = DIR;
    const root = host.attachShadow({ mode: 'closed' });
    const sheet = new CSSStyleSheet();
    sheet.replaceSync(CSS);
    root.adoptedStyleSheets = [sheet];
    document.documentElement.appendChild(host);
    return { host, root };
  }
  const el = (tag, cls, text) => {
    const e = document.createElement(tag);
    if (cls) e.className = cls;
    if (text != null) e.textContent = text;
    return e;
  };
  const head = text => {
    const h = el('div', 'head');
    h.append(el('span', 'logo', '2P'), el('span', '', text));
    return h;
  };
  /** 利用者の実際のクリックだけを受け付けるボタン */
  const button = (cls, onClick) => {
    const b = el('button', cls);
    b.type = 'button';
    b.addEventListener('mousedown', e => e.preventDefault()); // 入力欄のフォーカスを奪わない
    b.addEventListener('click', e => { if (e.isTrusted) onClick(); });
    return b;
  };

  const note = (cls, title, text) => {
    const n = el('div', 'note' + (cls ? ' ' + cls : ''));
    n.append(el('b', '', title), el('span', '', text));
    return n;
  };

  /** 右上に短いお知らせを出す（アプリでの確認が必要なときなど） */
  let notice = null;
  function showNotice(title, text, warn) {
    if (notice) notice.remove();
    const { host, root } = makeLayer();
    notice = host;
    const box = el('div', 'box banner');
    const h = el('h1');
    h.append(el('span', 'logo', '2P'), el('span', '', title));
    const p = el('p', '', text);
    p.style.whiteSpace = 'pre-line';
    if (warn) p.style.color = '#dc2626';
    box.append(h, p);
    root.append(box);
    setTimeout(() => { if (notice === host) { host.remove(); notice = null; } }, 9000);
  }
  /** パスワードを受け取れなかった理由を知らせる */
  function explain(res) {
    if (res.error === 'needs_approval') showNotice(t('approve_title'), t('approve_text'));
    else if (res.error === 'rate_limited') showNotice(t('rate_limited_title'), t('rate_limited_text'), true);
    else if (res.error === 'locked') showNotice(t('locked_title'), t('locked_text'));
  }

  // ---------------- 候補の一覧 ----------------

  let menu = null; // { host, field }
  let cache = null; // { at, res }
  const dismissed = new WeakSet(); // Esc で閉じた欄には、再フォーカスまで出さない

  function closeMenu() {
    if (menu) { menu.host.remove(); menu = null; }
  }

  async function listItems() {
    if (cache && Date.now() - cache.at < 15000) return cache.res;
    const res = await send({ type: 'list' });
    cache = { at: Date.now(), res };
    return res;
  }

  async function openMenu(field) {
    const targets = targetsFor(field);
    if (!targets || dismissed.has(field)) return;
    const res = await listItems();
    if (document.activeElement !== field) return;
    closeMenu();

    const rows = [];
    let title = '2password';
    if (res.ok) {
      for (const it of res.items) {
        const b = button('item', async () => {
          closeMenu();
          const got = await send({ type: 'fill', id: it.id });
          if (got.ok) {
            fill(targets, got.username, got.password);
            // 2 段階認証のコードを持つ項目なら、このあと出る認証コードの欄にも入力する
            if (got.totp) { otpArmed = true; setTimeout(tryOtp, 600); }
          } else { cache = null; explain(got); }
        });
        b.append(el('b', '', it.title), el('span', '', it.username || t('no_username')));
        rows.push(b);
      }
      // 新しいパスワードを決める欄では、強いパスワードの生成も出す
      const wantsNew = targets.pass && (field.autocomplete || '').includes('new-password');
      if (wantsNew) {
        const b = button('item', async () => {
          closeMenu();
          const got = await send({ type: 'generate' });
          if (!got.ok) return;
          inputsIn(scopeOf(field)).filter(isPassword).forEach(p => setValue(p, got.password));
        });
        b.append(el('b', '', t('generate_title')), el('span', '', t('generate_sub')));
        rows.push(b);
      }
      // 一致する項目がないとき：保存済みのサイトに似ていれば注意する（偽サイト対策）
      const rel = !res.items.length && res.related;
      if (rel && rel.kind === 'lookalike') {
        title = t('lookalike_title');
        rows.push(note('warn', t('lookalike_note_title', rel.host),
          t('lookalike_note_text', location.host, rel.host, rel.title)));
      } else if (rel && rel.kind === 'same_site' && targets.pass) {
        rows.push(note('', t('same_site_title', rel.host, rel.title), t('same_site_text', location.host)));
        const b = button('item', () => { closeMenu(); cache = null; send({ type: 'focus' }); });
        b.append(el('b', '', t('open_app')), el('span', '', t('open_app_sub_url')));
        rows.push(b);
      } else if (!res.items.length && res.idn && targets.pass) {
        title = t('idn_title');
        rows.push(note('warn', t('idn_note_title'), t('idn_note_text', location.host)));
      }
    } else if (res.error === 'insecure' && isPassword(field)) {
      title = '2password';
      rows.push(note('warn', t('insecure_title'), t('insecure_text')));
    } else if (res.error === 'locked' && isPassword(field)) {
      title = t('locked_title');
      const b = button('item', () => { closeMenu(); cache = null; send({ type: 'focus' }); });
      b.append(el('b', '', t('locked_menu_open')), el('span', '', t('locked_menu_sub')));
      rows.push(b);
    }
    if (rows.length) showMenu(field, title, rows);
  }

  function showMenu(field, title, rows) {
    closeMenu();
    const { host, root } = makeLayer();
    const box = el('div', 'box');
    const r = field.getBoundingClientRect();
    const width = Math.max(240, Math.min(r.width, 360));
    box.style.width = width + 'px';
    // 右から左に書く言語では、欄の右端にそろえる
    const start = DIR === 'rtl' ? r.right - width : r.left;
    box.style.left = Math.max(8, Math.min(start, innerWidth - width - 8)) + 'px';
    if (innerHeight - r.bottom > 180) box.style.top = r.bottom + 4 + 'px';
    else box.style.bottom = innerHeight - r.top + 4 + 'px';
    box.append(head(title), ...rows);
    root.append(box);
    menu = { host, field };
  }

  document.addEventListener('focusin', e => {
    const t = e.target;
    if (!(t instanceof HTMLInputElement) || !(isPassword(t) || isTextual(t) || t.type === 'number')) return;
    if (!targetsFor(t) && isOtp(t)) openOtpMenu(t);
    else openMenu(t);
  }, true);
  document.addEventListener('focusout', e => {
    dismissed.delete(e.target);
    if (menu && e.target === menu.field) { setTimeout(() => { if (menu && document.activeElement !== menu.field) closeMenu(); }, 150); }
  }, true);
  document.addEventListener('keydown', e => {
    if (e.key === 'Escape' && menu) { dismissed.add(menu.field); closeMenu(); }
  }, true);
  document.addEventListener('click', e => {
    // 一度閉じたあとでも、欄をクリックし直せば開く
    const t = e.target;
    if (e.isTrusted && !menu && t instanceof HTMLInputElement && dismissed.has(t)) {
      dismissed.delete(t);
      if (!targetsFor(t) && isOtp(t)) openOtpMenu(t);
      else openMenu(t);
    }
  }, true);
  addEventListener('scroll', closeMenu, { capture: true, passive: true });
  addEventListener('resize', closeMenu, { passive: true });

  // ポップアップで項目を選んだときの入力
  chrome.runtime.onMessage.addListener((msg, sender, sendResponse) => {
    if (sender.id !== chrome.runtime.id || !msg || msg.type !== 'fill-now') return;
    if (msg.origin !== location.origin) return sendResponse({ ok: false, error: 'navigated' });
    const active = document.activeElement;
    const targets = (active instanceof HTMLInputElement && targetsFor(active))
      || inputsIn(document).filter(isPassword).map(targetsFor)[0]
      || inputsIn(document).filter(looksLikeUsername).map(targetsFor).find(Boolean);
    if (targets && msg.totp) { otpArmed = true; setTimeout(tryOtp, 600); }
    sendResponse(targets && fill(targets, msg.username, msg.password)
      ? { ok: true }
      : { ok: false, error: 'no_form' });
  });

  // ---------------- 送信の検出と、保存の確認 ----------------

  function capture(scope) {
    const pws = inputsIn(scope).filter(p => isPassword(p) && p.value);
    if (!pws.length) {
      // パスワード欄のないページ（ID だけ先に聞くログイン）は、ユーザー名だけ覚えておく
      const u = inputsIn(scope).find(i => looksLikeUsername(i) && i.value);
      if (u && !inputsIn(scope).some(isPassword)) send({ type: 'username', username: u.value });
      return;
    }
    // 欄が複数あるとき（登録・パスワード変更）は、最後の欄＝新しいパスワードを採用する
    const pw = pws[pws.length - 1];
    const user = usernameFieldFor(pws[0]);
    send({ type: 'captured', username: user ? user.value : '', password: pw.value });
    // 画面が切り替わらないサイト（SPA）のために、少し待ってから確認を出す
    setTimeout(showPending, 1500);
  }

  document.addEventListener('submit', e => {
    if (e.target instanceof HTMLFormElement) capture(e.target);
  }, true);
  document.addEventListener('keydown', e => {
    if (e.isTrusted && e.key === 'Enter' && e.target instanceof HTMLInputElement && !e.target.form) capture(document);
  }, true);
  document.addEventListener('click', e => {
    if (!e.isTrusted || !(e.target instanceof Element)) return;
    const b = e.target.closest('button, input[type=submit], input[type=button], [role=button]');
    // form の送信ボタンは submit で拾う。ここでは form のないログイン画面を拾う
    if (b && !b.form && !b.closest('form')) capture(document);
  }, true);

  let banner = null;
  function closeBanner() {
    if (banner) { banner.remove(); banner = null; }
  }

  async function showPending() {
    const res = await send({ type: 'pending' });
    closeBanner();
    const p = res.ok && res.pending;
    if (!p) return;

    const { host, root } = makeLayer();
    banner = host;
    const box = el('div', 'box banner');
    const h = el('h1');
    const who = p.username ? t('who', p.username, p.host) : p.host;
    const row = el('div', 'row');
    const later = button('btn', () => { closeBanner(); send({ type: 'save-dismiss' }); });
    later.textContent = t('save_later');

    if (p.kind === 'locked') {
      h.append(el('span', 'logo', '2P'), el('span', '', t('save_title')));
      const open = button('btn primary', () => { send({ type: 'focus' }); setTimeout(showPending, 4000); });
      open.textContent = t('open_app');
      box.append(h, el('p', '', `${who}\n${t('save_locked_text')}`), row);
      row.append(later, open);
    } else {
      h.append(el('span', 'logo', '2P'), el('span', '', p.kind === 'update' ? t('update_title') : t('save_title')));
      const save = button('btn primary', async () => {
        save.textContent = t('saving');
        const done = await send({ type: 'save-confirm' });
        cache = null;
        h.lastChild.textContent = done.ok ? t('saved') : `${t('save_failed')}: ${errText(done)}`;
        row.remove();
        setTimeout(closeBanner, done.ok ? 1800 : 5000);
      });
      save.textContent = p.kind === 'update' ? t('update_button') : t('save_button');
      box.append(h, el('p', '', p.kind === 'update' ? `${who}\n${t('update_text', p.title)}` : who), row);
      row.append(later, save);
    }
    box.querySelector('p').style.whiteSpace = 'pre-line';
    root.append(box);
  }

  // ---------------- ワンタイムパスワード（2 段階認証のコード） ----------------
  //
  // 利用者が項目を選んでログインした直後（2 分以内）に、同じサイトで認証コードの欄が出たら、
  // その項目のコードを入力する。コードはデスクトップアプリが作り、設定キーは拡張機能に渡らない。
  // それ以外のときは、欄をクリックして項目を選んだときだけ入力する。

  const OTP_HINT = /otp|totp|2fa|mfa|two.?factor|one.?time|verif|auth.?code|security.?code|passcode|認証コード|確認コード|ワンタイム|セキュリティコード|验证码|驗證碼|código de verificación|código de verificação|codice di verifica|code de vérification|bestätigungscode|sicherheitscode|код подтверждения|doğrulama kodu|kod pengesahan|mã xác minh|รหัสยืนยัน|รหัส otp|सत्यापन कोड|رمز التحقق/i;
  function isOtp(el) {
    if (!(el instanceof HTMLInputElement) || !visible(el)) return false;
    if (!['text', 'tel', 'number', 'password', ''].includes(el.type || '')) return false;
    const ac = (el.autocomplete || '').toLowerCase();
    if (ac.includes('one-time-code')) return true;
    if (ac.includes('password') || ac.includes('username') || ac === 'email') return false;
    return OTP_HINT.test(`${el.name} ${el.id} ${el.placeholder} ${el.getAttribute('aria-label') || ''}`);
  }
  /** コードを入力する。1 文字ずつの欄が並んでいる画面にも対応する */
  function fillOtp(field, code) {
    const boxes = field.maxLength === 1 ? inputsIn(scopeOf(field)).filter(i => i.maxLength === 1) : [];
    if (boxes.length >= code.length) [...code].forEach((ch, i) => setValue(boxes[i], ch));
    else setValue(field, code);
  }

  const otpStart = Date.now();
  let otpArmed = false; // このページで項目を選んで入力した
  let otpAsked = false; // ページを開いたときの問い合わせを済ませた
  let otpBusy = false;
  async function tryOtp() {
    if (otpBusy || !(otpArmed || (!otpAsked && Date.now() - otpStart < 120000))) return;
    const field = inputsIn(document).find(isOtp);
    if (!field || field.value) return;
    otpBusy = true;
    const res = await send({ type: 'otp-auto' });
    otpBusy = false;
    otpAsked = true;
    otpArmed = false;
    if (res.ok) {
      if (!field.isConnected || field.value) return;
      fillOtp(field, res.code);
      showNotice(t('otp_filled_title'), t('otp_filled_text', location.host));
    } else if (res.error !== 'none') {
      explain(res);
    }
  }
  let otpTimer = null;
  new MutationObserver(() => {
    if (!otpArmed && otpAsked) return;
    clearTimeout(otpTimer);
    otpTimer = setTimeout(tryOtp, 400);
  }).observe(document.documentElement, { childList: true, subtree: true });
  setTimeout(tryOtp, 300);

  /** 認証コードの欄をクリックしたとき：コードを持つ項目を候補に出す */
  async function openOtpMenu(field) {
    if (dismissed.has(field)) return;
    const res = await listItems();
    if (document.activeElement !== field || !res.ok) return;
    const rows = res.items.filter(it => it.totp).map(it => {
      const b = button('item', async () => {
        closeMenu();
        const got = await send({ type: 'otp', id: it.id });
        if (got.ok) fillOtp(field, got.code);
        else { cache = null; explain(got); }
      });
      b.append(el('b', '', t('otp_fill')), el('span', '', [it.title, it.username].filter(Boolean).join(' · ')));
      return b;
    });
    if (rows.length) showMenu(field, '2password', rows);
  }

  // ---------------- パスキー ----------------
  //
  // ページ側（inpage.js）が navigator.credentials の呼び出しを知らせてくる。
  // ここで利用者に選んでもらい、選ばれたときだけデスクトップアプリに署名を頼む。
  // ページのスクリプトがこの知らせを偽造しても、出るのは同じ確認の画面で、
  // 署名に入るサイト名はブラウザが教えるものなので、ほかのサイトの署名は作れない。

  const PK_TAG = '__2password_passkey__';
  const pkSeen = new Set();
  let pkPrompt = null; // { host, id }

  const pkReply = (id, result) => postMessage({ [PK_TAG]: 'reply', id, result }, location.origin);
  function pkClose() {
    if (pkPrompt) { pkPrompt.host.remove(); pkPrompt = null; }
  }
  /** いま出している確認を「ブラウザに任せる」で終える */
  function pkGiveUp() {
    if (!pkPrompt) return;
    const { id } = pkPrompt;
    pkClose();
    pkReply(id, { action: 'native' });
  }

  /** 確認の画面を出す。サイトが開いているダイアログより手前に出す */
  function pkShow(id, title, text, rows, buttons) {
    if (pkPrompt && pkPrompt.id !== id) pkGiveUp();
    pkClose();
    const { host, root } = makeLayer();
    pkPrompt = { host, id };
    try { host.popover = 'manual'; host.showPopover(); } catch { /* 古いブラウザでは通常の重ね順で出す */ }
    const box = el('div', 'box banner');
    const h = el('h1');
    h.append(el('span', 'logo', '2P'), el('span', '', title));
    const p = el('p', '', text);
    p.style.whiteSpace = 'pre-line';
    const row = el('div', 'row');
    row.append(...buttons);
    box.append(h, p, ...rows, row);
    root.append(box);
  }
  const pkButton = (cls, label, onClick) => {
    const b = button(cls, onClick);
    b.textContent = label;
    return b;
  };
  const pkOther = id => pkButton('btn', t('pk_other'), () => { pkClose(); pkReply(id, { action: 'native' }); });

  /** ロック中：アプリを開いてもらい、解除されたら続きを行う */
  function pkLocked(id, retry) {
    const open = pkButton('btn primary', t('open_app'), async () => {
      open.textContent = t('pk_waiting');
      send({ type: 'focus' });
      for (let i = 0; i < 60; i++) {
        await new Promise(r => setTimeout(r, 1500));
        if (!pkPrompt || pkPrompt.id !== id) return;
        const st = await send({ type: 'status' });
        if (st.ok && !st.locked) return retry();
      }
      pkGiveUp();
    });
    pkShow(id, t('locked_title'), t('pk_locked_text'), [], [pkOther(id), open]);
  }

  async function pkCreate(id, d) {
    const st = await send({ type: 'status' });
    if (!st.ok || !st.configured) return pkReply(id, { action: 'native' });
    if (st.locked) return pkLocked(id, () => pkCreate(id, d));
    const save = pkButton('btn primary', t('pk_save'), async () => {
      save.textContent = t('pk_creating');
      const res = await send({ type: 'passkey-create', data: d });
      pkClose();
      if (res.ok) return pkReply(id, { action: 'done', credential: res });
      if (res.error === 'exists') return pkReply(id, { action: 'error', name: 'InvalidStateError', message: errText(res) });
      explain(res);
      if (res.error !== 'locked' && res.error !== 'rate_limited') showNotice(t('pk_save_failed'), `${errText(res)}\n${t('pk_save_failed_text')}`, true);
      pkReply(id, { action: 'native' });
    });
    const who = d.user_name ? t('who', d.user_name, location.host) : location.host;
    pkShow(id, t('pk_save_title'), `${who}\n${t('pk_save_text')}`, [], [pkOther(id), save]);
  }

  async function pkGet(id, d) {
    const res = await send({ type: 'passkey-list', rp_id: d.rp_id, allow: d.allow });
    if (!res.ok && res.error === 'locked') return pkLocked(id, () => pkGet(id, d));
    if (!res.ok || !res.items.length) return pkReply(id, { action: 'native' });
    const rows = res.items.map(it => {
      const b = button('item', async () => {
        const got = await send({ type: 'passkey-get', rp_id: d.rp_id, id: it.id, challenge: d.challenge });
        if (got.ok) { pkClose(); return pkReply(id, { action: 'done', credential: got }); }
        explain(got);
        // 初回の確認は、アプリで許可したあとにもう一度選べるよう、画面を残す
        if (got.error === 'needs_approval') return;
        pkClose();
        pkReply(id, got.error === 'rate_limited'
          ? { action: 'error', name: 'NotAllowedError', message: errText(got) }
          : { action: 'native' });
      });
      b.append(el('b', '', it.title), el('span', '', it.username || t('no_username')));
      return b;
    });
    pkShow(id, t('pk_login_title'), `${location.host}\n${t('pk_login_text')}`, rows, [pkOther(id)]);
  }

  addEventListener('message', e => {
    const m = e.data;
    if (e.source !== window || !m || typeof m.id !== 'string' || m.id.length > 100) return;
    if (m[PK_TAG] === 'cancel') {
      if (pkPrompt && pkPrompt.id === m.id) pkClose();
      return;
    }
    if (m[PK_TAG] !== 'request') return;
    // 受け取ったことを知らせる（ページ側は、これが届くまでくり返し送ってくる）
    postMessage({ [PK_TAG]: 'reply', id: m.id, ack: true }, location.origin);
    if (pkSeen.has(m.id)) return;
    pkSeen.add(m.id);
    const d = m.data && typeof m.data === 'object' ? m.data : {};
    (m.kind === 'create' ? pkCreate : pkGet)(m.id, d).catch(() => pkReply(m.id, { action: 'native' }));
  });

  // ログイン後に画面が切り替わった場合は、次のページで確認を出す
  showPending();
})();
