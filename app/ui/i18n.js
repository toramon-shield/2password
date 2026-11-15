// 2password GUI：表示言語の切り替え
//
// - 文言は locales/<言語>.json にある。"ui" は画面の文言、"rust" はアプリ本体（Rust）が返す
//   日本語のメッセージ（原文）とその訳
// - 言語は、利用者が設定で選んだもの、なければ OS の言語に合わせる
// - 訳がない文言は英語、それもなければ日本語の原文を出す
'use strict';

const I18N = (() => {
  const LANGS = {
    ja: '日本語', en: 'English', zh: '简体中文', pt: 'Português', es: 'Español', fr: 'Français',
    it: 'Italiano', de: 'Deutsch', ru: 'Русский', tr: 'Türkçe', ar: 'العربية', hi: 'हिन्दी',
    th: 'ไทย', vi: 'Tiếng Việt', ms: 'Bahasa Melayu',
  };
  const RTL = new Set(['ar']);
  const KEY = 'lang';

  let lang = 'ja';
  let ui = {};
  let fallback = {};
  let rust = {};
  let rustPatterns = [];

  /** OS（WebView）の言語から、対応している言語を選ぶ */
  function detect() {
    for (const l of navigator.languages || [navigator.language || 'en']) {
      const base = String(l).toLowerCase().split(/[-_]/)[0];
      if (base === 'nb' || base === 'no') continue;
      if (LANGS[base]) return base;
    }
    return 'en';
  }
  function saved() {
    try { return localStorage.getItem(KEY); } catch { return null; }
  }

  async function fetchJson(code) {
    try {
      const res = await fetch(`locales/${code}.json`);
      return res.ok ? await res.json() : null;
    } catch { return null; }
  }

  /** {name} を値で置き換える */
  function fill(text, vars) {
    return text.replace(/\{([a-zA-Z_][a-zA-Z0-9_]*)\}/g, (m, k) => (vars && k in vars ? String(vars[k]) : m));
  }

  function t(key, vars) {
    const s = ui[key] ?? fallback[key];
    return s == null ? key : fill(s, vars);
  }

  // Rust のメッセージ（原文）を訳す。{} や {name} の部分は、原文から取り出して訳に差し込む
  const SLOT = /\{[^{}]*\}/g;
  const escapeRe = s => s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  function buildPatterns(table) {
    const out = [];
    for (const [src, dst] of Object.entries(table)) {
      const slots = src.match(SLOT);
      if (!slots) continue;
      const re = new RegExp('^' + src.split(SLOT).map(escapeRe).join('([\\s\\S]*?)') + '$');
      out.push({ re, slots, dst });
    }
    return out;
  }
  function tr(message) {
    const text = String(message ?? '');
    return lang === 'ja' ? text : translateOne(text);
  }
  function translateOne(text) {
    if (text in rust) return rust[text];
    for (const p of rustPatterns) {
      const m = p.re.exec(text);
      if (!m) continue;
      const values = m.slice(1);
      const named = {};
      p.slots.forEach((s, i) => { if (s.length > 2) named[s] = values[i]; });
      let i = 0;
      return p.dst.replace(SLOT, s => (s.length > 2 && s in named ? named[s] : values[i++] ?? s));
    }
    return text;
  }

  /** data-i18n などの属性を持つ要素の文言を差し替える */
  function apply(root = document) {
    root.querySelectorAll('[data-i18n]').forEach(e => { e.textContent = t(e.dataset.i18n); });
    root.querySelectorAll('[data-i18n-html]').forEach(e => { e.innerHTML = t(e.dataset.i18nHtml); });
    for (const attr of ['placeholder', 'title', 'aria-label']) {
      const data = 'i18n' + attr.replace(/(^|-)([a-z])/g, (m, d, c) => c.toUpperCase());
      root.querySelectorAll(`[data-i18n-${attr}]`).forEach(e => e.setAttribute(attr, t(e.dataset[data])));
    }
  }

  async function init() {
    const choice = saved();
    lang = choice && LANGS[choice] ? choice : detect();
    const [main, en] = await Promise.all([fetchJson(lang), lang === 'en' ? null : fetchJson('en')]);
    const data = main || en || (await fetchJson('ja')) || { ui: {}, rust: {} };
    if (!main) lang = en ? 'en' : 'ja';
    ui = data.ui || {};
    fallback = (en && en.ui) || {};
    rust = { ...((en && en.rust) || {}), ...(data.rust || {}) };
    rustPatterns = buildPatterns(rust);
    document.documentElement.lang = lang;
    document.documentElement.dir = RTL.has(lang) ? 'rtl' : 'ltr';
    apply();
  }

  /** 言語の選択欄を作る（どの画面に置いても同じ動き） */
  function mountSelect(select) {
    const auto = document.createElement('option');
    auto.value = '';
    auto.textContent = t('lang.auto', { name: LANGS[detect()] });
    select.replaceChildren(auto, ...Object.entries(LANGS).map(([code, name]) => {
      const o = document.createElement('option');
      o.value = code;
      o.textContent = name;
      o.lang = code;
      return o;
    }));
    select.value = saved() || '';
    select.addEventListener('change', () => {
      try {
        if (select.value) localStorage.setItem(KEY, select.value);
        else localStorage.removeItem(KEY);
      } catch { /* 保存できなくても、この起動中は切り替える */ }
      // 保管庫のロック状態は Rust 側にあるので、画面を読み込み直しても解除のまま
      location.reload();
    });
  }

  return {
    init, t, tr, apply, mountSelect,
    get lang() { return lang; },
    /** 一覧を区切る記号（日本語なら「、」） */
    list: items => items.join(t('common.list_sep')),
  };
})();
const t = I18N.t;
const tr = I18N.tr;
