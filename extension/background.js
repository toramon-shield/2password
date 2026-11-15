// 2password ブラウザ拡張：バックグラウンド（service worker）
//
// - ページ内のスクリプト（content.js）とデスクトップアプリの間を取り次ぐ
// - アプリに渡す「どのサイトか」は、ページの申告ではなくブラウザが教える送信元（sender）から決める
// - 保管庫の鍵やアカウントパスワードは扱わない。パスワードは、利用者が選んだ項目の分だけ通過する
'use strict';

const HOST = 'ai.toramon.twopassword';
const PENDING_TTL_MS = 2 * 60 * 1000;

/** デスクトップアプリに 1 件の要求を送る */
async function native(request) {
  try {
    const res = await chrome.runtime.sendNativeMessage(HOST, request);
    return res || { ok: false, error: 'no_response' };
  } catch (e) {
    // ホストが登録されていない・アプリが見つからない
    return { ok: false, error: 'no_host', message: String(e && e.message || e) };
  }
}

/** 送信元のページのオリジン（最上位のフレームの http/https だけ） */
function senderOrigin(sender) {
  if (!sender.tab || sender.frameId !== 0) return null;
  const origin = sender.origin || (sender.url ? new URL(sender.url).origin : null);
  return origin && /^https?:\/\//.test(origin) ? origin : null;
}

const text = v => (typeof v === 'string' ? v.slice(0, 2048) : '');
const texts = v => (Array.isArray(v) ? v.slice(0, 64).map(text) : []);

const pendingKey = tabId => 'pending:' + tabId;
const userKey = tabId => 'user:' + tabId;
const otpKey = tabId => 'otp:' + tabId;

async function getFresh(key) {
  const v = (await chrome.storage.session.get(key))[key];
  if (!v) return null;
  if (Date.now() - v.at > PENDING_TTL_MS) {
    await chrome.storage.session.remove(key);
    return null;
  }
  return v;
}

/** 保存待ちのログイン情報について、ページに見せてよい情報（パスワードは含めない）を作る */
async function describePending(tabId) {
  const p = await getFresh(pendingKey(tabId));
  if (!p) return null;
  const res = await native({ type: 'check', origin: p.origin, username: p.username, password: p.password });
  const base = { host: new URL(p.origin).host, username: p.username };
  if (!res.ok) {
    if (res.error === 'locked') return { ...base, kind: 'locked' };
    // アプリが動いていないときは何も出さない（保存待ちは期限まで残す）
    return null;
  }
  if (res.result === 'new') return { ...base, kind: 'new' };
  if (res.result === 'update') {
    await chrome.storage.session.set({ [pendingKey(tabId)]: { ...p, updateId: res.id } });
    return { ...base, kind: 'update', title: res.title };
  }
  await chrome.storage.session.remove(pendingKey(tabId));
  return null;
}

async function onContentMessage(msg, sender) {
  const origin = senderOrigin(sender);
  if (!origin) return { ok: false, error: 'bad_origin' };
  const tabId = sender.tab.id;

  switch (msg.type) {
    case 'list':
      return native({ type: 'list', origin });
    case 'fill': {
      const res = await native({ type: 'fill', origin, id: String(msg.id) });
      // 2 段階認証のコードを持つ項目なら、続けて出る認証コードの欄のために、選んだ項目を覚えておく
      if (res.ok && res.totp) await chrome.storage.session.set({ [otpKey(tabId)]: { origin, id: String(msg.id), at: Date.now() } });
      return res;
    }
    // 直前にこのタブで選んだ項目の、いまのワンタイムパスワード（1 回だけ）
    case 'otp-auto': {
      const m = await getFresh(otpKey(tabId));
      if (!m || m.origin !== origin) return { ok: false, error: 'none' };
      const res = await native({ type: 'totp', origin, id: m.id });
      if (res.ok || res.error === 'no_match') await chrome.storage.session.remove(otpKey(tabId));
      return res;
    }
    // 利用者が認証コードの欄で項目を選んだ
    case 'otp':
      return native({ type: 'totp', origin, id: text(msg.id) });
    case 'approval':
      return native({ type: 'approval', origin, id: text(msg.id) });
    case 'generate':
      return native({ type: 'generate' });
    case 'focus':
      return native({ type: 'focus' });
    case 'status':
      return native({ type: 'status' });

    // パスキー：鍵と署名はアプリの中。ここは、ブラウザが教えるサイト名を添えて取り次ぐだけ
    case 'passkey-list':
      return native({ type: 'passkey_list', origin, rp_id: text(msg.rp_id), allow: texts(msg.allow) });
    case 'passkey-create': {
      const d = msg.data && typeof msg.data === 'object' ? msg.data : {};
      return native({
        type: 'passkey_create',
        origin,
        rp_id: text(d.rp_id),
        rp_name: text(d.rp_name),
        challenge: text(d.challenge),
        user_id: text(d.user_id),
        user_name: text(d.user_name),
        user_display_name: text(d.user_display_name),
        exclude: texts(d.exclude),
      });
    }
    case 'passkey-get':
      return native({ type: 'passkey_get', origin, rp_id: text(msg.rp_id), id: text(msg.id), challenge: text(msg.challenge) });

    // 複数ページに分かれたログイン（ID → パスワード）のために、直前のユーザー名を覚えておく
    case 'username':
      await chrome.storage.session.set({ [userKey(tabId)]: { origin, username: String(msg.username).slice(0, 512), at: Date.now() } });
      return { ok: true };

    case 'captured': {
      const password = String(msg.password || '');
      if (!password || password.length > 1024) return { ok: true };
      let username = String(msg.username || '').slice(0, 512);
      if (!username) {
        const u = await getFresh(userKey(tabId));
        if (u && u.origin === origin) username = u.username;
      }
      await chrome.storage.session.set({ [pendingKey(tabId)]: { origin, username, password, at: Date.now() } });
      return { ok: true };
    }
    case 'pending':
      return { ok: true, pending: await describePending(tabId) };

    case 'save-confirm': {
      const p = await getFresh(pendingKey(tabId));
      if (!p) return { ok: false, error: 'expired' };
      const res = p.updateId
        ? await native({ type: 'update', origin: p.origin, id: p.updateId, password: p.password })
        : await native({ type: 'save', origin: p.origin, username: p.username, password: p.password });
      if (res.ok) await chrome.storage.session.remove([pendingKey(tabId), userKey(tabId)]);
      return res;
    }
    case 'save-dismiss':
      await chrome.storage.session.remove(pendingKey(tabId));
      return { ok: true };
  }
  return { ok: false, error: 'bad_request' };
}

/** ポップアップ（拡張自身のページ）からの要求 */
async function onPopupMessage(msg) {
  switch (msg.type) {
    case 'status':
      return native({ type: 'status' });
    case 'focus':
      return native({ type: 'focus' });
    case 'list-tab':
    case 'fill-tab': {
      const tab = await chrome.tabs.get(msg.tabId);
      if (!tab.url || !/^https?:\/\//.test(tab.url)) return { ok: false, error: 'bad_origin' };
      const origin = new URL(tab.url).origin;
      if (msg.type === 'list-tab') return { ...(await native({ type: 'list', origin })), host: new URL(origin).host };
      const res = await native({ type: 'fill', origin, id: String(msg.id) });
      if (!res.ok) return res;
      // 入力はページ側のスクリプトに任せる。ページが切り替わっていたら入力しないよう、オリジンを添える
      try {
        if (res.totp) await chrome.storage.session.set({ [otpKey(tab.id)]: { origin, id: String(msg.id), at: Date.now() } });
        const done = await chrome.tabs.sendMessage(tab.id, { type: 'fill-now', origin, username: res.username, password: res.password, totp: Boolean(res.totp) }, { frameId: 0 });
        return done || { ok: false, error: 'no_form' };
      } catch {
        return { ok: false, error: 'no_content' };
      }
    }
  }
  return { ok: false, error: 'bad_request' };
}

chrome.runtime.onMessage.addListener((msg, sender, sendResponse) => {
  if (sender.id !== chrome.runtime.id || !msg || typeof msg.type !== 'string') return false;
  const fromPage = Boolean(sender.tab);
  const fromPopup = !fromPage && sender.url === chrome.runtime.getURL('popup.html');
  if (!fromPage && !fromPopup) return false;
  (fromPage ? onContentMessage(msg, sender) : onPopupMessage(msg))
    .then(sendResponse, e => sendResponse({ ok: false, error: 'failed', message: String(e) }));
  return true; // 非同期で応答する
});

// タブを閉じたら、そのタブの保存待ちを消す
chrome.tabs.onRemoved.addListener(tabId => chrome.storage.session.remove([pendingKey(tabId), userKey(tabId), otpKey(tabId)]));
