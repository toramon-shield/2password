// デスクトップアプリの画面の多言語表示の確認：
//   app/ui をヘッドレス Chrome で開き、Tauri の呼び出しを模擬して、言語ごとに主な画面を表示する
//   - 翻訳のキーがそのまま出ていないか、日本語が残っていないか
//   - アプリ本体（Rust）が返す日本語のメッセージが、表示言語に訳されるか
//   - アラビア語が右から左に表示されるか
//
// 使い方: cd scripts/browser-e2e && npm install && node ui-i18n.cjs [スクリーンショットの保存先]
const fs = require('fs');
const path = require('path');
const http = require('http');
const puppeteer = require('puppeteer-core');

const UI = path.resolve(__dirname, '../../app/ui');
const SHOTS = process.argv[2] ? path.resolve(process.argv[2]) : null;
const CHROME = process.env.CHROME_PATH || [
  'C:/Program Files/Google/Chrome/Application/chrome.exe',
  'C:/Program Files (x86)/Google/Chrome/Application/chrome.exe',
  '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
  '/usr/bin/google-chrome',
  '/usr/bin/chromium',
].find(p => fs.existsSync(p));

const ALL_LANGS = ['ja', 'en', 'zh', 'pt', 'es', 'fr', 'it', 'de', 'ru', 'tr', 'ar', 'hi', 'th', 'vi', 'ms'];
const CSP = JSON.parse(fs.readFileSync(path.resolve(__dirname, '../../app/src-tauri/tauri.conf.json'), 'utf8')).app.security.csp;
const LANGS = process.env.LANGS ? process.env.LANGS.split(',') : ALL_LANGS;
const JA = /[\u3040-\u30ff]/; // ひらがな・カタカナ（中国語の漢字と区別するため）
// Rust が返す日本語のメッセージ（差し込み口つき）。訳されて、差し込んだ値が残ること
const RUST_ERROR = '保管場所への保存に失敗しました（network down）。変更はこの端末に暗号化して保存済みで、次の保存・同期・解除のときに書き戻します。';
const RUST_NOTICE = '他の端末の変更を取り込みました（3 件）';

let pass = 0, fail = 0;
const check = (name, ok, extra) => {
  if (!ok || process.env.VERBOSE) console.log(`  ${ok ? '✓' : '✕'} ${name}${ok || !extra ? '' : ' :: ' + extra}`);
  ok ? pass++ : fail++;
};

function mockTauri(configured) {
  const item = {
    id: 'i1', title: 'Example', username: 'alice', urls: ['https://example.com'], notes: 'memo',
    updated: '2026-11-15 10:00 UTC', password_len: 12, has_totp: true,
    passkey: { rp_id: 'example.com', user_name: 'alice', created: '2026-11-15 10:00 UTC' },
  };
  const answers = {
    app_info: { configured, unlocked: configured, backend: 'drive', location: 'Google Drive', oauth_ready: true, detected_drive_folder: null, version: '0.5.0' },
    list_items: [{ id: 'i1', title: 'Example', username: 'alice', urls: ['https://example.com'], updated: '', has_passkey: true }],
    get_item: item,
    totp_code: { code: '123456', remaining: 17, period: 30 },
    status: { home: 'C:/x', backend: 'drive', location: 'Google Drive', aid: 'AB0123', known_counter: 7, files: [['account.2pw', 900], ['vault.2pw', null]], sample_hex: 'a1b2', oauth_ready: true },
    browser_status: { installed: true, dev_extension_id: 'fikflmlcdfanbjpplbnhlhnhbmefokoe', extra_extension_ids: [], approved_sites: 2 },
    browser_log: [{ at: 1763200000, event: 'filled', host: 'example.com', title: 'Example' }, { at: 1763200100, event: 'passkey_used', host: 'example.com', title: '' }],
    password_strength: 2,
  };
  return { answers, rustError: RUST_ERROR, rustNotice: RUST_NOTICE };
}

(async () => {
  const server = http.createServer((req, res) => {
    const file = path.join(UI, decodeURIComponent(req.url.split('?')[0]).replace(/^\/+/, '') || 'index.html');
    if (!file.startsWith(UI) || !fs.existsSync(file) || fs.statSync(file).isDirectory()) { res.statusCode = 404; return res.end(); }
    const type = { '.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css', '.json': 'application/json' }[path.extname(file)];
    res.setHeader('content-type', `${type}; charset=utf-8`);
    // 実際のアプリと同じ CSP をかける（翻訳ファイルの読み込みが止められないことを確かめる）
    res.setHeader('content-security-policy', CSP);
    res.end(fs.readFileSync(file));
  });
  await new Promise(done => server.listen(0, '127.0.0.1', done));
  const base = `http://127.0.0.1:${server.address().port}/index.html`;
  const browser = await puppeteer.launch({ executablePath: CHROME, headless: true, args: ['--no-first-run'] });
  if (SHOTS) fs.mkdirSync(SHOTS, { recursive: true });

  for (const lang of LANGS) {
    const keys = Object.keys(JSON.parse(fs.readFileSync(path.join(UI, 'locales', `${lang}.json`), 'utf8')).ui);
    for (const configured of [false, true]) {
      const page = await browser.newPage();
      await page.setViewport({ width: 1100, height: 760 });
      const errors = [];
      page.on('pageerror', e => errors.push(String(e)));
      await page.evaluateOnNewDocument((lang, mock) => {
        localStorage.setItem('lang', lang);
        const handlers = {};
        window.__TAURI__ = {
          core: {
            invoke: async (cmd) => {
              if (cmd === 'sync_now') throw mock.rustError;
              if (cmd in mock.answers) return JSON.parse(JSON.stringify(mock.answers[cmd]));
              return null;
            },
          },
          event: { listen: async (name, fn) => { handlers[name] = fn; } },
        };
        window.__notice = mock.rustNotice;
      }, lang, mockTauri(configured));
      await page.goto(base);
      await page.waitForFunction(() => [...document.querySelectorAll('.view')].some(v => !v.hidden && v.id !== 'v-loading'), { timeout: 8000 });

      if (configured) {
        // 項目を開き、編集・設定・確認の画面も表示する
        await page.click('#list button');
        await page.waitForSelector('#detail .field');
        await new Promise(r => setTimeout(r, 300));
        await page.click('#b-sync'); // Rust のエラーを返す
        await new Promise(r => setTimeout(r, 200));
        await page.evaluate(() => document.querySelector('#b-settings').click());
        await page.waitForFunction(() => document.querySelectorAll('#st-kv dt').length > 3);
        await page.evaluate(() => document.querySelectorAll('details').forEach(d => (d.open = true)));
      } else {
        await page.click('[data-go=create]');
      }
      await new Promise(r => setTimeout(r, 300));

      const seen = await page.evaluate(() => {
        const texts = [];
        const walk = n => {
          if (n.nodeType === 3 && n.textContent.trim()) texts.push(n.textContent.trim());
          if (n.nodeType === 1) {
            if (n.matches('script, style, template, option')) return;
            for (const a of ['placeholder', 'title', 'aria-label']) if (n.getAttribute(a)) texts.push(n.getAttribute(a));
            n.childNodes.forEach(walk);
          }
        };
        walk(document.body);
        return {
          texts,
          lang: document.documentElement.lang,
          dir: document.documentElement.dir,
          toast: [...document.querySelectorAll('.toast')].map(e => e.textContent),
          notice: typeof tr === 'function' ? tr(window.__notice) : '',
        };
      });
      const where = `${lang}/${configured ? 'main' : 'setup'}`;
      check(`${where}: スクリプトのエラーがない`, errors.length === 0, errors.join(' | '));
      check(`${where}: html の lang と dir`, seen.lang === lang && seen.dir === (lang === 'ar' ? 'rtl' : 'ltr'), `${seen.lang} ${seen.dir}`);
      const prefixes = new Set(keys.map(k => k.split('.')[0]));
      const rawKeys = seen.texts.filter(s => keys.includes(s) || (/^[a-z_]+\.[a-z_0-9]+$/.test(s) && prefixes.has(s.split('.')[0])));
      check(`${where}: 翻訳のキーがそのまま出ていない`, rawKeys.length === 0, rawKeys.join(', '));
      if (lang !== 'ja') {
        const left = seen.texts.filter(s => JA.test(s));
        check(`${where}: 日本語が残っていない`, left.length === 0, left.slice(0, 5).join(' | '));
      }
      if (configured) {
        const toast = seen.toast.join(' ');
        check(`${where}: Rust のエラーが訳され、差し込んだ値が残る`, toast.includes('network down') && (lang === 'ja' || !JA.test(toast)), toast);
        check(`${where}: Rust の通知が訳され、件数が残る`, seen.notice.includes('3') && (lang === 'ja' || !JA.test(seen.notice)), seen.notice);
      }
      if (SHOTS && configured) {
        await page.screenshot({ path: path.join(SHOTS, `${lang}-settings.png`) });
        await page.evaluate(() => document.querySelector('#d-settings').close());
        await page.screenshot({ path: path.join(SHOTS, `${lang}-main.png`) });
      }
      await page.close();
    }
  }
  await browser.close();
  server.close();
  console.log(`結果: 成功 ${pass} / 失敗 ${fail}`);
  process.exit(fail ? 1 : 0);
})().catch(e => { console.error('テストが中断しました:', e); process.exit(2); });
