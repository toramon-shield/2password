// ワンタイムパスワード（2 段階認証）の通し確認：
//   実際の Chrome（ヘッドレス）+ 拡張機能のスクリプト（content.js / background.js）
//   + 本物のコード生成（2pw bridge-request）+ Node.js で別に計算したコードとの比較
//
// 使い方（先に cargo build --release -p vault-cli を実行しておく）:
//   cd scripts/browser-e2e && npm install && npm test
const fs = require('fs');
const os = require('os');
const path = require('path');
const http = require('http');
const vm = require('vm');
const crypto = require('crypto');
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
const KEY = 'JBSWY3DPEHPK3PXP';
const work = fs.mkdtempSync(path.join(os.tmpdir(), '2pw-totp-'));
const env = { ...process.env, TWOPW_HOME: path.join(work, 'home') };
const cli = (args, input) => spawnSync(BIN, ['--password-stdin', ...args], { env, input, encoding: 'utf8' });
const sleep = ms => new Promise(r => setTimeout(r, ms));

let pass = 0, fail = 0;
const check = (name, ok, extra) => {
  console.log(`  ${ok ? '✓' : '✕'} ${name}${ok || !extra ? '' : ' :: ' + extra}`);
  ok ? pass++ : fail++;
};

/** 2password とは別の実装で計算したコード（RFC 6238、SHA-1、6 桁、30 秒）。前後の時間枠も許す */
function expectedCodes() {
  const alphabet = 'ABCDEFGHIJKLMNOPQRSTUVWXYZ234567';
  let bits = '';
  for (const c of KEY) bits += alphabet.indexOf(c).toString(2).padStart(5, '0');
  const key = Buffer.from(bits.match(/.{8}/g).map(b => parseInt(b, 2)));
  const at = step => {
    const counter = Buffer.alloc(8);
    counter.writeBigUInt64BE(BigInt(step));
    const mac = crypto.createHmac('sha1', key).update(counter).digest();
    const o = mac[mac.length - 1] & 15;
    return String((mac.readUInt32BE(o) & 0x7fffffff) % 1e6).padStart(6, '0');
  };
  const now = Math.floor(Date.now() / 30000);
  return [at(now - 1), at(now), at(now + 1)];
}

(async () => {
  let r = cli(['init', '--store', path.join(work, 'drive'), '--kit', path.join(work, 'kit.html')], `${PW}\n${PW}\n`);
  if (r.status !== 0) throw new Error('init failed: ' + r.stderr);

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
        if (approveNext && (req.type === 'fill' || req.type === 'totp')) { args.push('--approve'); approveNext = false; }
        const out = cli([...args, JSON.stringify(req)], PW + '\n');
        const line = out.stdout.trim().split('\n').pop();
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

  const content = '<script src="/content.js"></script>';
  const pages = {
    // 1 ページの中で、ログインのあとに認証コードの欄が出るサイト
    '/login': `<form id="f"><input id="u" name="username" autocomplete="username"><input id="p" type="password" autocomplete="current-password"><button id="go">ログイン</button></form>
      <script>document.getElementById('f').addEventListener('submit', e => { e.preventDefault();
        document.body.innerHTML = '<h1>2 段階認証</h1><input id="otp" autocomplete="one-time-code" inputmode="numeric">'; });</script>${content}`,
    // ログインのあとに別のページへ移り、1 文字ずつの欄が並ぶサイト
    '/mfa': `<h1>認証コード</h1><form>${[1, 2, 3, 4, 5, 6].map(i => `<input class="box" name="code${i}" maxlength="1" aria-label="認証コード ${i} 桁目" style="width:2em">`).join('')}</form>${content}`,
  };
  const server = http.createServer((req, res) => {
    if (req.url === '/content.js') {
      res.setHeader('content-type', 'text/javascript; charset=utf-8');
      return res.end(fs.readFileSync(path.join(EXT, 'content.js')));
    }
    res.setHeader('content-type', 'text/html; charset=utf-8');
    res.end(`<!doctype html><meta charset="utf-8"><title>site</title><body>${pages[req.url] || ''}`);
  });
  await new Promise(done => server.listen(0, 'localhost', done));
  const origin = `http://localhost:${server.address().port}`;

  r = cli(['add', 'Site', '--url', origin, '-u', 'alice'], `${PW}\nSite-P@ss-1\n`);
  if (r.status !== 0) throw new Error('add failed: ' + r.stderr);
  r = cli(['add', 'NoTotp', '--url', origin, '-u', 'bob'], `${PW}\nOther-P@ss-2\n`);
  r = cli(['totp', 'Site', '--set'], `${PW}\n${KEY.toLowerCase().replace(/(.{4})/g, '$1 ')}\n`);
  check('設定キーを登録すると、別の実装と同じコードが出る', r.status === 0 && expectedCodes().some(c => r.stdout.includes(c)), r.stdout + r.stderr);

  const browser = await puppeteer.launch({ executablePath: CHROME, headless: true, args: ['--no-first-run'] });
  const page = await browser.newPage();
  await page.setViewport({ width: 1000, height: 700 });
  await page.exposeFunction('__bg', msg => new Promise(resolve => {
    listener(msg, { id: 'ext', tab: { id: 1 }, frameId: 0, origin, url: origin + '/' }, resolve);
  }));
  // 拡張機能の文言（chrome.i18n）は、日本語の messages.json で模擬する
  const messages = JSON.parse(fs.readFileSync(path.join(EXT, '_locales', 'ja', 'messages.json'), 'utf8'));
  await page.evaluateOnNewDocument(messages => {
    const orig = Element.prototype.attachShadow;
    window.__roots = [];
    Element.prototype.attachShadow = function (o) { const root = orig.call(this, o); window.__roots.push(root); return root; };
    const i18n = {
      getMessage: (key, subs = []) => key === '@@bidi_dir' ? 'ltr'
        : messages[key] ? messages[key].message.replace(/\$([1-9])/g, (m, n) => subs[n - 1] ?? '') : '',
      getUILanguage: () => 'ja',
    };
    window.chrome = { runtime: { id: 'ext', sendMessage: m => window.__bg(m), onMessage: { addListener() {} } }, i18n };
  }, messages);

  async function clickIn(label, timeout = 8000) {
    const start = Date.now();
    for (;;) {
      const box = await page.evaluate(label => {
        for (const root of window.__roots) {
          if (!root.host.isConnected) continue;
          for (const b of root.querySelectorAll('button')) {
            if (b.textContent.includes(label)) {
              const q = b.getBoundingClientRect();
              return { x: q.left + q.width / 2, y: q.top + q.height / 2 };
            }
          }
        }
        return null;
      }, label);
      if (box) { await page.mouse.click(box.x, box.y); return; }
      if (Date.now() - start > timeout) throw new Error('ボタンが見つかりません: ' + label);
      await sleep(100);
    }
  }
  const shown = () => page.evaluate(() => window.__roots.filter(root => root.host.isConnected).map(root => root.textContent).join('|'));
  const value = sel => page.evaluate(sel => [...document.querySelectorAll(sel)].map(i => i.value).join(''), sel);
  const waitValue = async (sel, ms = 6000) => {
    for (const start = Date.now(); Date.now() - start < ms;) {
      const v = await value(sel);
      if (v) return v;
      await sleep(100);
    }
    return '';
  };

  // ---- 1. 1 ページの中で認証コードの欄が出るサイト ----
  await page.goto(origin + '/login');
  await page.click('#u');
  await clickIn('alice');
  await sleep(1200);
  check('初めてのサイトでは、アプリの確認なしに入力しない', (await value('#p')) === '' && (await shown()).includes('アプリで許可'));
  approveNext = true; // アプリの画面で「許可する」を押したことにする
  await page.click('#p');
  await clickIn('alice');
  check('項目を 1 回選ぶと、ユーザー名とパスワードが入る', (await waitValue('#p')) === 'Site-P@ss-1' && (await value('#u')) === 'alice');
  await page.click('#go');
  const code1 = await waitValue('#otp');
  check('続けて出た認証コードの欄に、クリックなしでコードが入る', expectedCodes().includes(code1), code1);
  check('入力したことを画面で知らせる', (await shown()).includes('ワンタイムパスワードを入力しました'));

  // ---- 2. 別のページへ移り、1 文字ずつの欄が並ぶサイト ----
  await page.goto(origin + '/login');
  await page.click('#u');
  await clickIn('alice');
  await waitValue('#p');
  await page.goto(origin + '/mfa');
  const code2 = await waitValue('.box:last-of-type');
  check('ページが切り替わっても、1 文字ずつの欄にコードが入る', expectedCodes().includes(await value('.box')), (await value('.box')) + '/' + code2);

  // ---- 3. 項目を選んでいないときは、自動では入力しない ----
  const before = nativeLog.length;
  await page.goto(origin + '/mfa');
  await sleep(2000);
  check('項目を選んでいないページでは、自動で入力しない', (await value('.box')) === '' && !nativeLog.slice(before).some(l => l.req.type === 'totp'));
  await page.click('.box');
  await clickIn('ワンタイムパスワードを入力');
  await waitValue('.box:last-of-type');
  check('認証コードの欄をクリックして項目を選べば入力できる', expectedCodes().includes(await value('.box')), await value('.box'));
  check('コードのない項目は、認証コードの候補に出さない', !(await page.evaluate(() => window.__roots.map(root => root.textContent).join('|'))).includes('bob'));

  // ---- 4. コードのない項目を選んだとき ----
  await page.goto(origin + '/login');
  await page.click('#u');
  approveNext = true;
  await clickIn('bob');
  await sleep(800);
  await page.click('#p');
  await clickIn('bob');
  await waitValue('#p');
  await page.click('#go');
  await sleep(2000);
  check('コードのない項目でログインしたときは、認証コードの欄に何も入れない', (await value('#otp')) === '', await value('#otp'));

  // ---- 5. 拡張機能に渡るもの ----
  const all = nativeLog.map(l => l.res).join('\n');
  check('拡張機能に設定キーが渡らない', !all.includes(KEY) && !/otpauth|secret/i.test(all));
  const raw = fs.readFileSync(path.join(work, 'drive', fs.readdirSync(path.join(work, 'drive')).find(f => f.startsWith('vault-'))));
  check('保存先のファイルに設定キーが平文で現れない', !raw.includes(KEY) && !raw.includes('otpauth'));

  await browser.close();
  server.close();
  fs.rmSync(work, { recursive: true, force: true });
  console.log(`結果: 成功 ${pass} / 失敗 ${fail}`);
  process.exit(fail ? 1 : 0);
})().catch(e => { console.error('テストが中断しました:', e); process.exit(2); });
