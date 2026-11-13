// 2password ブラウザ拡張：ツールバーのボタンを押したときの画面
'use strict';

const main = document.getElementById('main');
const send = msg => chrome.runtime.sendMessage(msg);

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
  const status = await send({ type: 'status' });
  if (!status.ok) {
    if (status.error === 'app_not_running') {
      return show(el('p', '', '2password アプリが起動していません。'), el('p', 'muted', 'アプリを起動してロックを解除してから、もう一度開いてください。'));
    }
    if (status.error === 'not_allowed') {
      return show(
        el('p', '', 'この拡張機能はアプリに登録されていません。'),
        steps(['2password アプリの「設定」を開く', '「拡張機能の ID を指定する」にこの ID を入力する', '「ブラウザと接続する」を押す']),
        el('p', 'muted', 'ID: ' + chrome.runtime.id),
      );
    }
    return show(
      el('p', '', '2password アプリと接続できていません。'),
      steps(['2password アプリを起動する', '「設定」→「ブラウザ拡張機能」→「ブラウザと接続する」を押す', 'ブラウザを再起動する']),
      el('p', 'muted', 'この拡張機能の ID: ' + chrome.runtime.id),
    );
  }
  if (!status.configured) {
    return show(el('p', '', '2password アプリで保管庫を作成してください。'), openAppButton('アプリを開く'));
  }
  if (status.locked) {
    return show(el('p', '', '2password はロック中です。'), openAppButton('アプリを開いてロックを解除'));
  }

  const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
  const list = tab ? await send({ type: 'list-tab', tabId: tab.id }) : { ok: false, error: 'bad_origin' };
  if (!list.ok) {
    return show(el('p', 'muted', list.error === 'bad_origin' ? 'このページでは使えません。' : (list.message || '一覧を取得できませんでした。')));
  }
  document.getElementById('host').textContent = list.host;
  if (!list.items.length) {
    return show(
      el('p', '', 'このサイトの項目はまだありません。'),
      el('p', 'muted', 'ログインすると、保存するかどうかを確認します。アプリの「＋ 追加」で登録するときは、URL 欄にこのサイトのアドレスを入れてください。'),
    );
  }
  const error = el('p', 'error');
  const rows = list.items.map(it => {
    const b = el('button', 'item');
    b.append(el('b', '', it.title), el('span', '', it.username || '（ユーザー名なし）'));
    b.addEventListener('click', async () => {
      const done = await send({ type: 'fill-tab', tabId: tab.id, id: it.id });
      if (done.ok) window.close();
      else error.textContent = done.message || '入力できませんでした。';
    });
    return b;
  });
  show(el('p', 'muted', 'クリックすると、このページに入力します。'), ...rows, error);
}

render().catch(e => show(el('p', 'error', String(e))));
