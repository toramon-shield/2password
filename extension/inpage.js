// 2password ブラウザ拡張：ページと同じ世界で動く部分（パスキー）
//
// サイトがパスキーを作る・使うとき（navigator.credentials.create / get）に、
// 2password を保管場所として選べるようにする。
//
// - ここにあるのは「取り次ぎ」だけ。鍵はデスクトップアプリの中にあり、このスクリプトには渡らない
// - 利用者が 2password を選ばなかったとき、アプリが動いていないとき、対応していない要求のときは、
//   ブラウザのもとの動作にそのまま戻す
// - サイト名（origin）はここでは決めない。ブラウザが拡張機能に教えるものを使う
(() => {
  'use strict';
  if (window.top !== window) return; // 最上位のフレームだけ
  const creds = navigator.credentials;
  if (!creds || typeof PublicKeyCredential === 'undefined') return;

  const nativeCreate = creds.create.bind(creds);
  const nativeGet = creds.get.bind(creds);
  const TAG = '__2password_passkey__';
  const ACK_WAIT_MS = 1500; // 拡張機能の応答を待つ時間。なければブラウザの動作に戻す

  // ---------------- 変換 ----------------

  const toBytes = v => {
    if (v instanceof ArrayBuffer) return new Uint8Array(v);
    if (ArrayBuffer.isView(v)) return new Uint8Array(v.buffer, v.byteOffset, v.byteLength);
    throw new TypeError('BufferSource が必要です');
  };
  const b64 = v => {
    let s = '';
    for (const b of toBytes(v)) s += String.fromCharCode(b);
    return btoa(s).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
  };
  const unb64 = text => {
    const s = atob(text.replace(/-/g, '+').replace(/_/g, '/'));
    const out = new Uint8Array(s.length);
    for (let i = 0; i < s.length; i++) out[i] = s.charCodeAt(i);
    return out.buffer;
  };

  // ---------------- 拡張機能とのやりとり ----------------

  let seq = 0;
  /** 拡張機能に取り次ぎを頼む。返り値は { action: 'native' | 'done' | 'error', ... } */
  function ask(kind, data, signal) {
    return new Promise(resolve => {
      const id = `${Date.now()}-${++seq}-${Math.random().toString(36).slice(2)}`;
      let acked = false;
      const finish = result => {
        removeEventListener('message', onMessage);
        clearInterval(retry);
        clearTimeout(giveUp);
        if (signal) signal.removeEventListener('abort', onAbort);
        resolve(result);
      };
      const onMessage = e => {
        const m = e.data;
        if (e.source !== window || !m || m[TAG] !== 'reply' || m.id !== id) return;
        if (m.ack) { acked = true; clearInterval(retry); clearTimeout(giveUp); return; }
        finish(m.result || { action: 'native' });
      };
      const onAbort = () => {
        postMessage({ [TAG]: 'cancel', id }, location.origin);
        finish({ action: 'aborted' });
      };
      const post = () => postMessage({ [TAG]: 'request', id, kind, data }, location.origin);
      addEventListener('message', onMessage);
      if (signal) signal.addEventListener('abort', onAbort, { once: true });
      // ページの読み込み直後は、拡張機能の受け口がまだ用意できていないことがあるので、くり返し送る
      const retry = setInterval(() => { if (!acked) post(); }, 200);
      const giveUp = setTimeout(() => { if (!acked) finish({ action: 'native' }); }, ACK_WAIT_MS);
      post();
    });
  }

  // ---------------- サイトに返すオブジェクト ----------------

  function withPrototype(obj, proto) {
    try { Object.setPrototypeOf(obj, proto); } catch { /* そのまま返す */ }
    return obj;
  }

  function attestationCredential(c, extensions) {
    const clientDataJSON = unb64(c.client_data_json);
    const attestationObject = unb64(c.attestation_object);
    const authenticatorData = unb64(c.authenticator_data);
    const publicKey = unb64(c.public_key);
    const transports = ['internal', 'hybrid'];
    const response = withPrototype({
      clientDataJSON,
      attestationObject,
      getTransports: () => transports.slice(),
      getAuthenticatorData: () => authenticatorData,
      getPublicKey: () => publicKey,
      getPublicKeyAlgorithm: () => -7,
    }, AuthenticatorAttestationResponse.prototype);
    const results = extensions && extensions.credProps ? { credProps: { rk: true } } : {};
    return withPrototype({
      id: c.credential_id,
      rawId: unb64(c.credential_id),
      type: 'public-key',
      authenticatorAttachment: 'platform',
      response,
      getClientExtensionResults: () => results,
      toJSON: () => ({
        id: c.credential_id,
        rawId: c.credential_id,
        type: 'public-key',
        authenticatorAttachment: 'platform',
        clientExtensionResults: results,
        response: {
          clientDataJSON: c.client_data_json,
          attestationObject: c.attestation_object,
          authenticatorData: c.authenticator_data,
          publicKey: c.public_key,
          publicKeyAlgorithm: -7,
          transports,
        },
      }),
    }, PublicKeyCredential.prototype);
  }

  function assertionCredential(c) {
    const response = withPrototype({
      clientDataJSON: unb64(c.client_data_json),
      authenticatorData: unb64(c.authenticator_data),
      signature: unb64(c.signature),
      userHandle: c.user_handle ? unb64(c.user_handle) : null,
    }, AuthenticatorAssertionResponse.prototype);
    return withPrototype({
      id: c.credential_id,
      rawId: unb64(c.credential_id),
      type: 'public-key',
      authenticatorAttachment: 'platform',
      response,
      getClientExtensionResults: () => ({}),
      toJSON: () => ({
        id: c.credential_id,
        rawId: c.credential_id,
        type: 'public-key',
        authenticatorAttachment: 'platform',
        clientExtensionResults: {},
        response: {
          clientDataJSON: c.client_data_json,
          authenticatorData: c.authenticator_data,
          signature: c.signature,
          userHandle: c.user_handle || undefined,
        },
      }),
    }, PublicKeyCredential.prototype);
  }

  const ids = list => (Array.isArray(list) ? list : [])
    .filter(d => d && (d.type === undefined || d.type === 'public-key'))
    .map(d => b64(d.id));

  // ---------------- 置き換え ----------------

  async function create(options) {
    const pk = options && options.publicKey;
    let data;
    try {
      // ES256（-7）を受け付けないサイトには対応していない
      const algs = (pk.pubKeyCredParams || []).map(p => p.alg);
      if (!pk || !algs.includes(-7)) return nativeCreate(options);
      // 鍵（セキュリティキー）を名指ししている場合は、ブラウザに任せる
      if (pk.authenticatorSelection && pk.authenticatorSelection.authenticatorAttachment === 'cross-platform') return nativeCreate(options);
      data = {
        rp_id: (pk.rp && pk.rp.id) || location.hostname,
        rp_name: (pk.rp && pk.rp.name) || '',
        challenge: b64(pk.challenge),
        user_id: b64(pk.user.id),
        user_name: String(pk.user.name || ''),
        user_display_name: String(pk.user.displayName || ''),
        exclude: ids(pk.excludeCredentials),
      };
    } catch {
      return nativeCreate(options);
    }
    const r = await ask('create', data, options.signal);
    if (r.action === 'done') return attestationCredential(r.credential, pk.extensions);
    if (r.action === 'aborted') throw options.signal.reason || new DOMException('中止されました', 'AbortError');
    if (r.action === 'error') throw new DOMException(r.message || '操作できませんでした', r.name || 'NotAllowedError');
    return nativeCreate(options);
  }

  async function get(options) {
    const pk = options && options.publicKey;
    let data;
    try {
      // 入力欄の候補に出す方式（conditional）は、ブラウザに任せる
      if (!pk || options.mediation === 'conditional') return nativeGet(options);
      data = {
        rp_id: pk.rpId || location.hostname,
        challenge: b64(pk.challenge),
        allow: ids(pk.allowCredentials),
      };
    } catch {
      return nativeGet(options);
    }
    const r = await ask('get', data, options.signal);
    if (r.action === 'done') return assertionCredential(r.credential);
    if (r.action === 'aborted') throw options.signal.reason || new DOMException('中止されました', 'AbortError');
    if (r.action === 'error') throw new DOMException(r.message || '操作できませんでした', r.name || 'NotAllowedError');
    return nativeGet(options);
  }

  // もとの関数と同じように見えるよう、同じ名前の関数として差し替える
  const wrap = (name, fn) => Object.defineProperty(creds, name, {
    configurable: true,
    writable: true,
    value: { [name](options) { return fn(options); } }[name],
  });
  wrap('create', options => (options && options.publicKey ? create(options) : nativeCreate(options)));
  wrap('get', options => (options && options.publicKey ? get(options) : nativeGet(options)));
})();
