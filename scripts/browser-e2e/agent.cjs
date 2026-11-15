// AI エージェント（Claude Code・Codex など）がブラウザを操作するときの通し確認：
//   実際の Chrome（ヘッドレス）+ 拡張機能のスクリプト（content.js / background.js）+ 本物の保管庫（2pw bridge-request）
//   - エージェントと同じく、DevTools プロトコルのクリック・キー入力と、アクセシビリティツリーで候補を見つけて選ぶ
//   - アプリでの許可を待ってから入力する（選び直さなくてよい）
//   - 「入力のたびに確認する」設定では毎回確認し、拒否されたら入力しない
//   - パスワードはアクセシビリティツリーに出ない。ページのスクリプトが作ったキー入力では選ばれない
//
// 使い方（先に cargo build --release -p vault-cli を実行しておく）:
//   cd scripts/browser-e2e && npm install && node agent.cjs
const fs = require('fs');
const os = require('os');
const path = require('path');
const http = require('http');
const vm = require('vm');
const { spawnSync } = require('child_process');
const puppeteer = require('puppeteer-core');

const EXT = path.resolve(process.argv[2] || '../../extension');
const BIN = path.resolve((process.argv[3] || '../../target/release/2pw') + (process.platform === 'win32' && !/[.]exe$/.test(process.argv[3] || '') ? '.exe' : ''));
const CHROME = process.argv[4] || process.env.CHROME_PATH || [
  'C:/Program Files/Google/Chrome/Application/chrome.exe',
  'C:/Program Files (x86)/Google/Chrome/Application/chrome.exe',
  '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
  '/usr/bin/google-chrome',
  '/usr/bin/chromium',
].find(p => fs.existsSync(p));

const PW = 'blue whale coffee mountain 2026';
const SITE_PW = 'Agent-P@ss-42';
const work = fs.mkdtempSync(path.join(os.tmpdir(), '2pw-agent-'));
const env = { ...process.env, TWOPW_HOME: path.join(work, 'home'), TWOPW_LANG: 'ja' };
const cli = (args, input) => spawnSync(BIN, ['--password-stdin', ...args], { env, input, encoding: 'utf8' });
const sleep = ms => new Promise(r => setTimeout(r, ms));

let pass = 0, fail = 0;
const check = (name, ok, extra) => {
  console.log(`  ${ok ? '✓' : '✕'} ${name}${ok || !extra ? '' : ' :: ' + extra}`);
  ok ? pass++ : fail++;
};

(async () => {
  let r = cli(['init', '--store', path.join(work, 'drive'), '--kit', path.join(work, 'kit.html')], `${PW}\n${PW}\n`);
  if (r.status !== 0) throw new Error('init failed: ' + r.stderr);

  // アプリの画面の代わり：確認の状態をテストから決める
  const app = { state: 'none', confirmEach: false, approveNext: false, asked: 0 };
  let listener;
  const session = {};
  const chromeStub = {
    runtime: {
      id: 'ext',
      getURL: p => 'chrome-extension://ext/' + p,
      onMessage: { addListener: fn => { listener = fn; } },
      sendNativeMessage: async (_host, req) => {
        if (req.type === 'approval') return { ok: true, state: app.state };
        const args = ['bridge-request'];
        if (app.confirmEach) args.push('--confirm-each');
        if (app.approveNext && ['fill', 'totp'].includes(req.type)) { args.push('--approve'); app.approveNext = false; }
        const out = cli([...args, JSON.stringify(req)], PW + '\n');
        const res = JSON.parse(out.stdout.trim().split('\n').pop());
        if (res.error === 'needs_approval') { app.asked++; app.state = 'pending'; }
        return res;
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
  /** アプリの画面で「許可する」「拒否」を押したことにする */
  const answer = allow => { app.state = allow ? 'allowed' : 'denied'; app.approveNext = allow; };

  const pages = {
    '/login': `<h1>サインイン</h1><form id="f" action="/done"><label>メール <input id="u" name="email" autocomplete="username"></label>
      <label>パスワード <input id="p" type="password" autocomplete="current-password"></label><button id="go">ログイン</button></form>
      <script src="/content.js"></script>`,
    '/done': 'ok',
  };
  const server = http.createServer((req, res) => {
    if (req.url === '/content.js') {
      res.setHeader('content-type', 'text/javascript; charset=utf-8');
      return res.end(fs.readFileSync(path.join(EXT, 'content.js')));
    }
    res.setHeader('content-type', 'text/html; charset=utf-8');
    res.end(`<!doctype html><meta charset="utf-8"><title>site</title><body>${pages[req.url.split('?')[0]] || ''}`);
  });
  await new Promise(done => server.listen(0, 'localhost', done));
  const origin = `http://localhost:${server.address().port}`;
  r = cli(['add', 'Agent Site', '--url', origin, '-u', 'agent@example.com'], `${PW}\n${SITE_PW}\n`);
  if (r.status !== 0) throw new Error('add failed: ' + r.stderr);

  const browser = await puppeteer.launch({ executablePath: CHROME, headless: true, args: ['--no-first-run'] });
  const page = await browser.newPage();
  await page.setViewport({ width: 1000, height: 700 });
  await page.exposeFunction('__bg', msg => new Promise(resolve => {
    listener(msg, { id: 'ext', tab: { id: 1 }, frameId: 0, origin, url: origin + '/' }, resolve);
  }));
  const messages = JSON.parse(fs.readFileSync(path.join(EXT, '_locales', 'ja', 'messages.json'), 'utf8'));
  await page.evaluateOnNewDocument(messages => {
    const i18n = {
      getMessage: (key, subs = []) => key === '@@bidi_dir' ? 'ltr'
        : messages[key] ? messages[key].message.replace(/\$([1-9])/g, (m, n) => subs[n - 1] ?? '') : '',
      getUILanguage: () => 'ja',
    };
    window.chrome = { runtime: { id: 'ext', sendMessage: m => window.__bg(m), onMessage: { addListener() {} } }, i18n };
  }, messages);

  const value = sel => page.$eval(sel, i => i.value);
  const waitFor = async (fn, ms = 8000) => {
    for (const start = Date.now(); Date.now() - start < ms; await sleep(100)) if (await fn()) return true;
    return false;
  };
  /** アクセシビリティツリー（エージェントが読むもの）の文字をすべて集める */
  async function axText() {
    const snap = await page.accessibility.snapshot({ interestingOnly: false });
    const out = [];
    const walk = n => { if (!n) return; out.push(n.name || '', n.value || '', n.description || ''); (n.children || []).forEach(walk); };
    walk(snap);
    return out.join('\n');
  }
  /** エージェントと同じく、読み上げ用の名前で候補を探してクリックする（DevTools プロトコルのマウス操作） */
  async function clickByName(name) {
    let h = null;
    await waitFor(async () => (h = await page.$(`aria/${name}`)) !== null);
    if (!h) throw new Error('見つかりません: ' + name);
    await h.click();
  }
  const LABEL = 'Agent Site（agent@example.com）';

  // ---- 1. 初めてのサイト：アプリで許可されるまで待ち、選び直さずに入力する ----
  await page.goto(origin + '/login');
  await page.click('#u'); // DevTools プロトコルのクリック（Claude in Chrome などと同じ）
  check('候補がアクセシビリティツリーに名前つきで出る', await waitFor(async () => (await axText()).includes(`2password で入力: ${LABEL}`)));
  await clickByName(`2password で入力: ${LABEL}`);
  check('初めてのサイトでは、許可を待つお知らせを出し、まだ入力しない',
    await waitFor(async () => app.state === 'pending' && (await axText()).includes('2password アプリで許可してください')) && (await value('#p')) === '');
  await sleep(1500);
  check('待っている間はパスワードを受け取らない', (await value('#p')) === '');
  answer(true);
  check('許可されたら、選び直さなくても入力する', await waitFor(async () => (await value('#p')) === SITE_PW) && (await value('#u')) === 'agent@example.com');
  check('入力したことを画面で知らせる（エージェントが確かめられる）', await waitFor(async () => (await axText()).includes('2password で入力しました')));
  const tree = await axText();
  check('パスワードはアクセシビリティツリーに出ない', !tree.includes(SITE_PW), tree.split('\n').filter(s => s.includes('P@ss')).join(' | '));

  // ---- 2. キーボードで選ぶ（↓・Enter）。フォームは送信しない ----
  app.state = 'none';
  await page.goto(origin + '/login');
  await page.click('#p');
  await waitFor(async () => (await axText()).includes(`2password で入力: ${LABEL}`));
  await page.keyboard.press('ArrowDown');
  await page.keyboard.press('Enter');
  check('↓ と Enter で選ぶと入力する（許可済みのサイトは確認なし）', await waitFor(async () => (await value('#p')) === SITE_PW));
  check('Enter でフォームを送信しない', new URL(page.url()).pathname === '/login');

  // ---- 3. ページのスクリプトが作ったキー入力では選ばない ----
  await page.goto(origin + '/login');
  await page.click('#u');
  await waitFor(async () => (await axText()).includes(`2password で入力: ${LABEL}`));
  await page.evaluate(() => {
    const u = document.getElementById('u');
    for (const key of ['ArrowDown', 'Enter']) u.dispatchEvent(new KeyboardEvent('keydown', { key, bubbles: true }));
  });
  await sleep(1500);
  check('ページのスクリプトが作ったキー入力では入力しない', (await value('#p')) === '');

  // ---- 4. 入力のたびに確認する設定 ----
  app.confirmEach = true;
  app.state = 'none';
  const askedBefore = app.asked;
  await page.goto(origin + '/login');
  await page.click('#u');
  await clickByName(`2password で入力: ${LABEL}`);
  check('入力のたびに確認する設定では、許可済みのサイトでも確認する', await waitFor(async () => app.asked > askedBefore) && (await value('#p')) === '');
  answer(false);
  check('拒否されたら入力せず、そのことを知らせる', await waitFor(async () => (await axText()).includes('2password アプリで拒否されました')) && (await value('#p')) === '');
  app.state = 'none';
  await page.goto(origin + '/login');
  await page.click('#u');
  await clickByName(`2password で入力: ${LABEL}`);
  await waitFor(async () => app.state === 'pending');
  answer(true);
  check('入力のたびに確認する設定でも、許可すれば入力する', await waitFor(async () => (await value('#p')) === SITE_PW));

  await browser.close();
  server.close();
  console.log(`結果: 成功 ${pass} / 失敗 ${fail}`);
  process.exit(fail ? 1 : 0);
})().catch(e => { console.error('テストが中断しました:', e); process.exit(2); });
