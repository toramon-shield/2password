// 2password ブラウザ拡張：ページの中で動く部分
//
// - ログイン欄にフォーカスが当たったら、このサイトに一致する項目を候補として出す
// - 利用者が候補をクリックしたときだけ、ユーザー名とパスワードを入力する（自動では入力しない）
// - ログインや登録の送信を見つけたら、次の画面で「保存しますか？」と確認する
// - 表示する部品は closed な Shadow DOM の中に作り、ページのスクリプトから読めないようにする
(() => {
  'use strict';
  if (window.top !== window) return; // 最上位のフレームだけ

  const send = msg => chrome.runtime.sendMessage(msg).catch(() => ({ ok: false, error: 'no_background' }));

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
    .banner { top: 16px; right: 16px; width: 320px; padding: 14px; }
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
    if (warn) p.style.color = '#dc2626';
    box.append(h, p);
    root.append(box);
    setTimeout(() => { if (notice === host) { host.remove(); notice = null; } }, 9000);
  }
  /** パスワードを受け取れなかった理由を知らせる */
  function explain(res) {
    if (res.error === 'needs_approval') showNotice('2password アプリで許可してください', 'このサイトで使うのは初めてです。アプリの画面で許可してから、もう一度項目を選んでください。');
    else if (res.error === 'rate_limited') showNotice('2password をロックしました', res.message || '短い時間に要求が続きました。', true);
    else if (res.error === 'locked') showNotice('2password はロック中です', 'アプリでロックを解除してください。');
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
          if (got.ok) fill(targets, got.username, got.password);
          else { cache = null; explain(got); }
        });
        b.append(el('b', '', it.title), el('span', '', it.username || '（ユーザー名なし）'));
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
        b.append(el('b', '', '強いパスワードを作って入力'), el('span', '', '送信後に、保存するか確認します'));
        rows.push(b);
      }
      // 一致する項目がないとき：保存済みのサイトに似ていれば注意する（偽サイト対策）
      const rel = !res.items.length && res.related;
      if (rel && rel.kind === 'lookalike') {
        title = '注意：保存済みのサイトとは別のサイトです';
        rows.push(note('warn', `「${rel.host}」に似ていますが、別のサイトです`,
          `このページは ${location.host} です。${rel.host}（${rel.title}）のパスワードを、ここに入力しないでください。アドレスをよく確かめてください。`));
      } else if (rel && rel.kind === 'same_site' && targets.pass) {
        rows.push(note('', `${rel.host}（${rel.title}）とはアドレスが違います`,
          `このページは ${location.host} です。ここでも使うには、アプリで項目の URL にこのアドレスを追加してください。`));
        const b = button('item', () => { closeMenu(); cache = null; send({ type: 'focus' }); });
        b.append(el('b', '', 'アプリを開く'), el('span', '', '項目の「編集」から URL を追加できます'));
        rows.push(b);
      } else if (!res.items.length && res.idn && targets.pass) {
        title = '注意：アドレスを確かめてください';
        rows.push(note('warn', 'アドレスに特殊な文字が含まれています',
          `このページ（${location.host}）は、見た目がよく似た別の文字を使った偽サイトの可能性があります。`));
      }
    } else if (res.error === 'insecure' && isPassword(field)) {
      title = '2password';
      rows.push(note('warn', '暗号化されていないページです（http）',
        '通信の途中でパスワードを読まれるおそれがあるため、このページには入力しません。'));
    } else if (res.error === 'locked' && isPassword(field)) {
      title = '2password はロック中です';
      const b = button('item', () => { closeMenu(); cache = null; send({ type: 'focus' }); });
      b.append(el('b', '', 'アプリを開いてロックを解除'), el('span', '', '解除したら、もう一度この欄をクリックしてください'));
      rows.push(b);
    }
    if (!rows.length) return;

    const { host, root } = makeLayer();
    const box = el('div', 'box');
    const r = field.getBoundingClientRect();
    const width = Math.max(240, Math.min(r.width, 360));
    box.style.width = width + 'px';
    box.style.left = Math.max(8, Math.min(r.left, innerWidth - width - 8)) + 'px';
    if (innerHeight - r.bottom > 180) box.style.top = r.bottom + 4 + 'px';
    else box.style.bottom = innerHeight - r.top + 4 + 'px';
    box.append(head(title), ...rows);
    root.append(box);
    menu = { host, field };
  }

  document.addEventListener('focusin', e => {
    const t = e.target;
    if (t instanceof HTMLInputElement && (isPassword(t) || isTextual(t))) openMenu(t);
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
    if (e.isTrusted && !menu && t instanceof HTMLInputElement && dismissed.has(t)) { dismissed.delete(t); openMenu(t); }
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
    sendResponse(targets && fill(targets, msg.username, msg.password)
      ? { ok: true }
      : { ok: false, error: 'no_form', message: 'このページに入力欄が見つかりません' });
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
    const who = p.username ? `${p.username}（${p.host}）` : p.host;
    const row = el('div', 'row');
    const later = button('btn', () => { closeBanner(); send({ type: 'save-dismiss' }); });
    later.textContent = '今回はしない';

    if (p.kind === 'locked') {
      h.append(el('span', 'logo', '2P'), el('span', '', 'ログイン情報を保存しますか？'));
      const open = button('btn primary', () => { send({ type: 'focus' }); setTimeout(showPending, 4000); });
      open.textContent = 'アプリを開く';
      box.append(h, el('p', '', `${who}\n2password がロック中です。解除すると保存できます。`), row);
      row.append(later, open);
    } else {
      h.append(el('span', 'logo', '2P'), el('span', '', p.kind === 'update' ? 'パスワードを更新しますか？' : 'ログイン情報を保存しますか？'));
      const save = button('btn primary', async () => {
        save.textContent = '保存中…';
        const done = await send({ type: 'save-confirm' });
        cache = null;
        h.lastChild.textContent = done.ok ? '2password に保存しました' : (done.message || '保存できませんでした');
        row.remove();
        setTimeout(closeBanner, done.ok ? 1800 : 5000);
      });
      save.textContent = p.kind === 'update' ? '更新する' : '保存する';
      box.append(h, el('p', '', p.kind === 'update' ? `${who}\n「${p.title}」のパスワードを新しいものに置き換えます。` : who), row);
      row.append(later, save);
    }
    box.querySelector('p').style.whiteSpace = 'pre-line';
    root.append(box);
  }

  // ログイン後に画面が切り替わった場合は、次のページで確認を出す
  showPending();
})();
