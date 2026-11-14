// パスキーの通し確認：
//   実際の Chrome（ヘッドレス）+ 拡張機能のスクリプト（inpage.js / content.js / background.js）
//   + 本物の署名処理（2pw bridge-request）+ 独立した検証ライブラリ（@simplewebauthn/server）
//
// 使い方（先に cargo build --release -p vault-cli を実行しておく）:
//   cd scripts/passkey-e2e && npm install && npm test
// 実際の Google Drive にも、利用者の保管庫にも触れない（一時フォルダに試験用の保管庫を作る）。
const fs = require('fs');
const os = require('os');
const path = require('path');
const http = require('http');
const vm = require('vm');
const { spawnSync } = require('child_process');
const puppeteer = require('puppeteer-core');
const swa = require('@simplewebauthn/server');

const EXT = path.resolve(process.argv[2] || '../../extension');
const BIN = path.resolve((process.argv[3] || '../../target/release/2pw') + (process.platform === 'win32' && !/[.]exe$/.test(process.argv[3] || '') ? '.exe' : ''));
// Chrome の場所：引数、環境変数 CHROME_PATH、よくある場所の順に探す
const CHROME = process.argv[4] || process.env.CHROME_PATH || [
  'C:/Program Files/Google/Chrome/Application/chrome.exe',
  'C:/Program Files (x86)/Google/Chrome/Application/chrome.exe',
  '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
  '/usr/bin/google-chrome',
  '/usr/bin/chromium',
].find(p => fs.existsSync(p));
const PW = 'blue whale coffee mountain 2026';
const work = fs.mkdtempSync(path.join(os.tmpdir(), '2pw-pk-'));
const env = { ...process.env, TWOPW_HOME: path.join(work, 'home') };
const cli = (args, input) => spawnSync(BIN, ['--password-stdin', ...args], { env, input, encoding: 'utf8' });

let pass = 0, fail = 0;
const check = (name, ok, extra) => {
  console.log(`  ${ok ? '✓' : '✕'} ${name}${ok || !extra ? '' : ' :: ' + extra}`);
  ok ? pass++ : fail++;
};

(async () => {
  const init = cli(['init', '--store', path.join(work, 'drive'), '--kit', path.join(work, 'kit.html')], `${PW}\n${PW}\n`);
  if (init.status !== 0) throw new Error('init failed: ' + init.stderr);

  // ---- background.js を、偽の chrome オブジェクトの上で動かす ----
  let listener;
  const session = {};
  let approveNext = false;
  const nativeLog = [];
  const chromeStub = {
    runtime: {
      id: 'ext',
      getURL: p => 'chrome-extension://ext/' + p,
      onMessage: { addListener: fn => { listener = fn; } },
      sendNativeMessage: async (_host, req) => {
        const args = ['bridge-request'];
        if (approveNext) { args.push('--approve'); approveNext = false; }
        const r = cli([...args, JSON.stringify(req)], PW + '\n');
        const line = r.stdout.trim().split('\n').pop();
        nativeLog.push({ req, res: line });
        return JSON.parse(line);
      },
    },
    storage: { session: {
      get: async k => ({ [k]: session[k] }),
      set: async o => { Object.assign(session, o); },
      remove: async k => { for (const x of [].concat(k)) delete session[x]; },
    } },
    tabs: { onRemoved: { addListener() {} }, get: async () => ({}) },
  };
  vm.runInNewContext(fs.readFileSync(path.join(EXT, 'background.js'), 'utf8'), { chrome: chromeStub, URL, console, Date, setTimeout });

  // ---- テスト用のサイト（http://localhost は WebAuthn を使える） ----
  const server = http.createServer((req, res) => {
    if (req.url === '/inpage.js' || req.url === '/content.js') {
      res.setHeader('content-type', 'text/javascript; charset=utf-8');
      return res.end(fs.readFileSync(path.join(EXT, req.url.slice(1))));
    }
    res.setHeader('content-type', 'text/html; charset=utf-8');
    res.end('<!doctype html><meta charset="utf-8"><title>rp</title><script src="/inpage.js"></script><body><h1>RP</h1><script src="/content.js"></script>');
  });
  await new Promise(r => server.listen(0, 'localhost', r));
  const origin = `http://localhost:${server.address().port}`;
  const rpID = 'localhost';

  const browser = await puppeteer.launch({ executablePath: CHROME, headless: true, args: ['--no-first-run'] });
  const page = await browser.newPage();
  await page.setViewport({ width: 1000, height: 700 });
  // ブラウザが拡張機能に教える「送信元」を、ここで再現する
  await page.exposeFunction('__bg', msg => new Promise(resolve => {
    listener(msg, { id: 'ext', tab: { id: 1 }, frameId: 0, origin, url: origin + '/' }, resolve);
  }));
  await page.evaluateOnNewDocument(() => {
    // テストから中身を見られるよう、closed の Shadow DOM を記録する（製品の動作は変えない）
    const orig = Element.prototype.attachShadow;
    window.__roots = [];
    Element.prototype.attachShadow = function (o) { const r = orig.call(this, o); window.__roots.push(r); return r; };
    window.chrome = { runtime: { id: 'ext', sendMessage: m => window.__bg(m), onMessage: { addListener() {} } } };
  });
  // ブラウザ標準のパスキー（「ほかの方法」の先）を、仮想の認証器で受ける
  const cdp = await page.createCDPSession();
  await cdp.send('WebAuthn.enable');
  await cdp.send('WebAuthn.addVirtualAuthenticator', { options: {
    protocol: 'ctap2', transport: 'internal', hasResidentKey: true, hasUserVerification: true, isUserVerified: true, automaticPresenceSimulation: true,
  } });
  await page.goto(origin + '/');

  /** 2password の確認の画面にあるボタンを、本物のマウス操作で押す */
  async function clickIn(label, timeout = 8000) {
    const start = Date.now();
    for (;;) {
      const box = await page.evaluate(label => {
        for (const r of window.__roots) {
          if (!r.host.isConnected) continue;
          for (const b of r.querySelectorAll('button')) {
            if (b.textContent.includes(label)) {
              const q = b.getBoundingClientRect();
              return { x: q.left + q.width / 2, y: q.top + q.height / 2, top: document.elementFromPoint(q.left + q.width / 2, q.top + q.height / 2) === r.host };
            }
          }
        }
        return null;
      }, label);
      if (box) { await page.mouse.click(box.x, box.y); return box; }
      if (Date.now() - start > timeout) throw new Error('ボタンが見つかりません: ' + label);
      await new Promise(r => setTimeout(r, 100));
    }
  }
  const promptText = () => page.evaluate(() => window.__roots.filter(r => r.host.isConnected).map(r => r.textContent).join('|'));

  /** ページの中で navigator.credentials を呼び、結果を JSON で受け取る（押すボタンは並行して操作する） */
  const callCreate = options => page.evaluate(async json => {
    try {
      const c = await navigator.credentials.create({ publicKey: PublicKeyCredential.parseCreationOptionsFromJSON(json) });
      return { ok: true, json: c.toJSON(), isPkc: c instanceof PublicKeyCredential, isAtt: c.response instanceof AuthenticatorAttestationResponse,
        alg: c.response.getPublicKeyAlgorithm(), transports: c.response.getTransports(), ext: c.getClientExtensionResults() };
    } catch (e) { return { ok: false, name: e.name, message: e.message }; }
  }, options);
  const callGet = options => page.evaluate(async json => {
    try {
      const c = await navigator.credentials.get({ publicKey: PublicKeyCredential.parseRequestOptionsFromJSON(json) });
      return { ok: true, json: c.toJSON(), isPkc: c instanceof PublicKeyCredential, isAss: c.response instanceof AuthenticatorAssertionResponse };
    } catch (e) { return { ok: false, name: e.name, message: e.message }; }
  }, options);

  // ---- 1. 登録 ----
  const regOpts = await swa.generateRegistrationOptions({
    rpName: 'テスト用サイト', rpID, userName: 'alice@example.com', userDisplayName: 'Alice',
    authenticatorSelection: { residentKey: 'required', userVerification: 'required' }, extensions: { credProps: true },
  });
  let pending = callCreate(regOpts);
  const saveBtn = await clickIn('2password に保存');
  check('作成の確認が、ページのいちばん手前に出る', saveBtn.top);
  const reg = await pending;
  check('サイトにパスキーの作成結果が返る', reg.ok, JSON.stringify(reg));
  check('返したものが PublicKeyCredential として扱える', reg.isPkc && reg.isAtt && reg.alg === -7 && reg.ext.credProps && reg.ext.credProps.rk === true);
  let regInfo;
  try {
    const v = await swa.verifyRegistrationResponse({ response: reg.json, expectedChallenge: regOpts.challenge, expectedOrigin: origin, expectedRPID: rpID, requireUserVerification: true });
    regInfo = v.registrationInfo;
    check('独立した検証ライブラリが、登録を正しいと認める', v.verified, JSON.stringify(v));
    check('同期できるパスキーとして登録される', regInfo.credentialDeviceType === 'multiDevice' && regInfo.credentialBackedUp === true);
  } catch (e) { check('独立した検証ライブラリが、登録を正しいと認める', false, String(e)); }

  const list = cli(['list'], PW + '\n').stdout;
  check('保管庫に項目ができる', list.includes('alice@example.com') && list.includes('テスト用サイト'), list);
  const created = nativeLog.find(l => l.req.type === 'passkey_create');
  check('拡張機能に秘密鍵が渡らない', created && !/private|pkcs8/i.test(created.res) && !Object.keys(JSON.parse(created.res)).includes('private_key'));
  const raw = fs.readFileSync(path.join(work, 'drive', fs.readdirSync(path.join(work, 'drive')).find(f => f.startsWith('vault-'))));
  check('保存先のファイルに、サイト名や資格情報 ID が平文で現れない', !raw.includes('alice@example.com') && !raw.includes(reg.json.id));

  // ---- 2. ログイン ----
  if (regInfo) {
    const authOpts = await swa.generateAuthenticationOptions({ rpID, userVerification: 'required' });
    pending = callGet(authOpts);
    await clickIn('alice@example.com');
    const got = await pending;
    check('サイトにログインの署名が返る', got.ok && got.isPkc && got.isAss, JSON.stringify(got));
    try {
      const v = await swa.verifyAuthenticationResponse({ response: got.json, expectedChallenge: authOpts.challenge, expectedOrigin: origin, expectedRPID: rpID,
        credential: regInfo.credential, requireUserVerification: true });
      check('独立した検証ライブラリが、署名を正しいと認める', v.verified, JSON.stringify(v));
    } catch (e) { check('独立した検証ライブラリが、署名を正しいと認める', false, String(e)); }
    try {
      await swa.verifyAuthenticationResponse({ response: got.json, expectedChallenge: authOpts.challenge, expectedOrigin: 'https://evil.test', expectedRPID: rpID, credential: regInfo.credential });
      check('署名は別のサイト名では通らない', false);
    } catch { check('署名は別のサイト名では通らない', true); }

    // 資格情報 ID を指定したログイン
    const authOpts2 = await swa.generateAuthenticationOptions({ rpID, allowCredentials: [{ id: regInfo.credential.id }] });
    pending = callGet(authOpts2);
    await clickIn('alice@example.com');
    const got2 = await pending;
    const v2 = got2.ok && await swa.verifyAuthenticationResponse({ response: got2.json, expectedChallenge: authOpts2.challenge, expectedOrigin: origin, expectedRPID: rpID, credential: regInfo.credential }).catch(() => null);
    check('資格情報 ID を指定したログインも通る', Boolean(v2 && v2.verified));

    // ---- 3. 同じパスキーの二重登録 ----
    const regOpts2 = await swa.generateRegistrationOptions({ rpName: 'テスト用サイト', rpID, userName: 'alice@example.com', excludeCredentials: [{ id: regInfo.credential.id }] });
    pending = callCreate(regOpts2);
    await clickIn('2password に保存');
    const dup = await pending;
    check('登録済みのパスキーは、もう一度作らない（InvalidStateError）', !dup.ok && dup.name === 'InvalidStateError', JSON.stringify(dup));

    // ---- 4. 2password を選ばないとき ----
    const authOpts3 = await swa.generateAuthenticationOptions({ rpID, allowCredentials: [{ id: 'AAAAAAAAAAAAAAAAAAAAAA' }] });
    const before = nativeLog.length;
    const none = await Promise.race([callGet(authOpts3), new Promise(r => setTimeout(() => r({ timeout: true }), 6000))]);
    check('持っていないパスキーの要求は、確認を出さずにブラウザへ渡す', !nativeLog.slice(before).some(l => l.req.type === 'passkey_get') && !(await promptText()).includes('パスキーでログイン'), JSON.stringify(none));
    await page.reload();
  }
  const regOpts3 = await swa.generateRegistrationOptions({ rpName: 'テスト用サイト', rpID, userName: 'bob@example.com' });
  pending = callCreate(regOpts3);
  await clickIn('ほかの方法を使う');
  const native = await pending;
  const nv = native.ok && await swa.verifyRegistrationResponse({ response: native.json, expectedChallenge: regOpts3.challenge, expectedOrigin: origin, expectedRPID: rpID, requireUserVerification: false }).catch(() => null);
  check('「ほかの方法」を選ぶと、ブラウザ標準のパスキーに切り替わる', Boolean(nv && nv.verified) && !cli(['list'], PW + '\n').stdout.includes('bob@example.com'), JSON.stringify(native).slice(0, 200));

  // ---- 5. 対応していない要求 ----
  const condBefore = nativeLog.length;
  const cond = await page.evaluate(async () => {
    const before = window.__roots.length;
    const ac = new AbortController();
    const p = navigator.credentials.get({ mediation: 'conditional', signal: ac.signal, publicKey: { challenge: new Uint8Array(16), rpId: 'localhost' } }).catch(e => e.name);
    await new Promise(r => setTimeout(r, 800));
    ac.abort();
    await p;
    return { prompts: window.__roots.length - before };
  });
  check('入力欄の候補方式（conditional）には手を出さない', cond.prompts === 0 && nativeLog.length === condBefore, JSON.stringify(cond));
  const pwOnly = await page.evaluate(() => navigator.credentials.get({ password: true, mediation: 'silent' }).then(c => c === null, e => e.name));
  check('パスキー以外の要求は、もとの動作のまま', pwOnly === true, String(pwOnly));

  // ---- 6. 初めてのサイトでの確認（アプリで登録を取り消したあと） ----
  fs.rmSync(path.join(work, 'home', 'approved-sites.json'), { force: true });
  if (regInfo) {
    await page.reload();
    const authOpts4 = await swa.generateAuthenticationOptions({ rpID });
    pending = callGet(authOpts4);
    await clickIn('alice@example.com');
    await new Promise(r => setTimeout(r, 1500));
    const text = await promptText();
    const asked = nativeLog.filter(l => l.req.type === 'passkey_get').pop();
    check('初めてのサイトでは、アプリの確認なしに署名しない', asked.res.includes('needs_approval') && !asked.res.includes('signature') && text.includes('アプリで許可'), text.slice(0, 120));
    approveNext = true; // アプリの画面で「許可する」を押したことにする
    await clickIn('alice@example.com');
    const got = await pending;
    const v = got.ok && await swa.verifyAuthenticationResponse({ response: got.json, expectedChallenge: authOpts4.challenge, expectedOrigin: origin, expectedRPID: rpID, credential: regInfo.credential }).catch(() => null);
    check('許可したあとは、もう一度選ぶとログインできる', Boolean(v && v.verified), JSON.stringify(got).slice(0, 200));
  }

  await browser.close();
  server.close();
  fs.rmSync(work, { recursive: true, force: true });
  console.log(`結果: 成功 ${pass} / 失敗 ${fail}`);
  process.exit(fail ? 1 : 0);
})().catch(e => { console.error('テストが中断しました:', e); process.exit(2); });
