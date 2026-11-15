// 2password ブラウザ拡張：ツールバーのボタンを押したときの画面
// 文言は _locales/<言語>/messages.json にある（ブラウザの表示言語で選ばれる）
'use strict';

const main = document.getElementById('main');
const send = msg => chrome.runtime.sendMessage(msg);
const t = (key, ...subs) => chrome.i18n.getMessage(key, subs.map(String)) || key;
/** アプリから返ったエラーを、コードから訳す */
const errText = res => chrome.i18n.getMessage('err_' + (res && res.error)) || t('err_failed');

document.documentElement.lang = chrome.i18n.getUILanguage();
document.documentElement.dir = chrome.i18n.getMessage('@@bidi_dir') || 'ltr';

function el(tag, cls, text) {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text != null) e.textContent = text;
  return e;
}
function show(...nodes) {
  main.replaceChildren(...nodes);
}
function steps(lines) {
  const ol = el('ol');
  lines.forEach(l => ol.append(el('li', '', l)));
  return ol;
}
function openAppButton(label) {
  const b = el('button', 'primary', label);
  b.addEventListener('click', async () => { await send({ type: 'focus' }); window.close(); });
  return b;
}

async function render() {
  show(el('p', 'muted', t('popup_checking')));
  const status = await send({ type: 'status' });
  if (!status.ok) {
    if (status.error === 'app_not_running') {
      return show(el('p', '', t('popup_not_running')), el('p', 'muted', t('popup_not_running_text')));
    }
    if (status.error === 'not_allowed') {
      return show(
        el('p', '', t('popup_not_allowed')),
        steps([t('popup_not_allowed_step1'), t('popup_not_allowed_step2'), t('popup_not_allowed_step3')]),
        el('p', 'muted', t('popup_id', chrome.runtime.id)),
      );
    }
    return show(
      el('p', '', t('popup_no_host')),
      steps([t('popup_no_host_step1'), t('popup_no_host_step2'), t('popup_no_host_step3')]),
      el('p', 'muted', t('popup_ext_id', chrome.runtime.id)),
    );
  }
  if (!status.configured) {
    return show(el('p', '', t('popup_not_configured')), openAppButton(t('open_app')));
  }
  if (status.locked) {
    return show(el('p', '', t('popup_locked')), openAppButton(t('popup_unlock')));
  }

  const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
  const list = tab ? await send({ type: 'list-tab', tabId: tab.id }) : { ok: false, error: 'bad_origin' };
  if (!list.ok) {
    return show(el('p', 'muted', errText(list)));
  }
  document.getElementById('host').textContent = list.host;
  if (!list.items.length && list.related && list.related.kind === 'lookalike') {
    return show(
      el('p', 'error', t('popup_lookalike', list.related.host)),
      el('p', 'muted', t('popup_lookalike_text', list.related.host, list.related.title)),
    );
  }
  if (!list.items.length && list.related) {
    return show(
      el('p', '', t('popup_same_site', list.related.host, list.related.title)),
      el('p', 'muted', t('popup_same_site_text')),
      openAppButton(t('open_app')),
    );
  }
  if (!list.items.length) {
    return show(el('p', '', t('popup_empty')), el('p', 'muted', t('popup_empty_text')));
  }
  const error = el('p', 'error');
  const rows = list.items.map(it => {
    const b = el('button', 'item');
    b.append(el('b', '', it.title), el('span', '', it.username || t('no_username')));
    b.addEventListener('click', async () => {
      const done = await send({ type: 'fill-tab', tabId: tab.id, id: it.id });
      if (done.ok) window.close();
      else error.textContent = errText(done);
    });
    return b;
  });
  show(el('p', 'muted', t('popup_click_to_fill')), ...rows, error);
}

render().catch(e => show(el('p', 'error', String(e))));
