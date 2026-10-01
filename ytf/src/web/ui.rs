/// Одностраничный интерфейс. Без сборки — просто строка.
pub const PAGE: &str = r##"<!doctype html>
<html lang="ru"><head>
<meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>YTF — YouTube пайплайн</title>
<style>
:root{
  --bg:#0d1117; --bg2:#161b22; --bg3:#1c2128; --bd:#30363d;
  --tx:#e6edf3; --mu:#8b949e; --ac:#58a6ff;
  --ok:#3fb950; --err:#f85149; --warn:#d29922;
}
*{box-sizing:border-box;margin:0;padding:0}
body{background:var(--bg);color:var(--tx);font:14px/1.5 -apple-system,Segoe UI,system-ui,sans-serif}
header{background:var(--bg2);border-bottom:1px solid var(--bd);padding:12px 20px;display:flex;align-items:center;gap:16px;position:sticky;top:0;z-index:10}
header h1{font-size:16px;font-weight:600}
header .sp{flex:1}
.badge{font-size:11px;padding:3px 8px;border-radius:12px;background:var(--bg3);color:var(--mu);border:1px solid var(--bd)}
.badge.ok{color:var(--ok);border-color:var(--ok)}
.badge.err{color:var(--err);border-color:var(--err)}
.badge.busy{color:var(--warn);border-color:var(--warn)}
nav{display:flex;gap:2px;padding:0 20px;background:var(--bg2);border-bottom:1px solid var(--bd)}
nav button{background:none;border:none;border-bottom:2px solid transparent;color:var(--mu);padding:10px 14px;cursor:pointer;font-size:13px;font-family:inherit}
nav button:hover{color:var(--tx)}
nav button.on{color:var(--tx);border-bottom-color:var(--ac)}
main{padding:20px;max-width:1400px}
.tab{display:none}.tab.on{display:block}
h2{font-size:15px;margin-bottom:12px}
.card{background:var(--bg2);border:1px solid var(--bd);border-radius:8px;padding:16px;margin-bottom:16px}
.row{display:flex;gap:12px;flex-wrap:wrap;align-items:center}
button.b{background:var(--ac);color:#0d1117;border:none;padding:8px 14px;border-radius:6px;font-weight:600;cursor:pointer;font-size:13px;font-family:inherit}
button.b:disabled{opacity:.5;cursor:not-allowed}
button.g{background:var(--bg3);color:var(--tx);border:1px solid var(--bd)}
button.g:hover{border-color:var(--mu)}
table{width:100%;border-collapse:collapse;font-size:13px}
th,td{padding:8px 10px;text-align:left;border-bottom:1px solid var(--bd);vertical-align:top}
th{color:var(--mu);font-weight:600;font-size:11px;text-transform:uppercase;letter-spacing:.4px}
tr:last-child td{border-bottom:none}
input,select{background:var(--bg);border:1px solid var(--bd);color:var(--tx);padding:7px 10px;border-radius:6px;font-family:inherit;font-size:13px}
input[type=number]{width:90px}
label{color:var(--mu);font-size:12px;display:block;margin-bottom:4px}
.g2{display:grid;grid-template-columns:repeat(auto-fit,minmax(280px,1fr));gap:16px}
#log{background:#010409;border:1px solid var(--bd);border-radius:6px;padding:12px;font:12px/1.6 ui-monospace,Menlo,Consolas,monospace;max-height:380px;overflow-y:auto;white-space:pre-wrap;color:#adbac7}
.pill{font-size:11px;padding:2px 7px;border-radius:10px;background:var(--bg3);color:var(--mu)}
.pill.y{color:var(--ok);background:#1a2f1a}
.pill.n{color:var(--err);background:#2d1618}
.pill.w{color:var(--warn);background:#2d2410}
.mono{font:12px ui-monospace,Menlo,Consolas,monospace;color:var(--mu)}
a{color:var(--ac)}
.bar{height:6px;background:var(--bg3);border-radius:3px;overflow:hidden;min-width:90px}
.bar i{display:block;height:100%;background:var(--ac)}
.muted{color:var(--mu);font-size:12px}
.err-t{color:var(--err);font-size:12px}
</style></head><body>

<header>
  <h1>YTF</h1>
  <span class="badge" id="q-ins">—</span>
  <span class="badge" id="q-gen">—</span>
  <span class="sp"></span>
  <span class="badge" id="st">idle</span>
</header>

<nav>
  <button class="on" data-t="ch">Каналы</button>
  <button data-t="pub">Публикация</button>
  <button data-t="set">Настройки</button>
  <button data-t="his">Журнал</button>
  <button data-t="tok">Токены</button>
</nav>

<main>

<section class="tab on" id="t-ch">
  <div class="card">
    <div class="row"><h2 style="margin:0">Каналы</h2><span class="sp"></span>
      <button class="b g" onclick="load()">Обновить</button></div>
  </div>
  <div class="card" style="padding:0;overflow:hidden">
    <table><thead><tr>
      <th>Человек</th><th>Папка</th><th>Канал на YT</th><th>Режим</th>
      <th>Треки</th><th>Картинки</th><th>Видео</th><th>Использовано</th><th>Токен</th><th></th>
    </tr></thead><tbody id="chb"><tr><td colspan="10" class="muted">загрузка…</td></tr></tbody></table>
  </div>
</section>

<section class="tab" id="t-pub">
  <div class="card">
    <h2>Опубликовать</h2>
    <div class="g2">
      <div>
        <label>Канал</label>
        <select id="pch" style="width:100%"><option value="">все каналы</option></select>
      </div>
      <div>
        <label>Горизонт, дней вперёд</label>
        <input type="number" id="phz" value="1" min="1" max="7">
      </div>
      <div>
        <label>Воркеров (пусто = авто)</label>
        <input type="number" id="pw" placeholder="авто" min="1" max="16">
      </div>
      <div style="display:flex;align-items:flex-end;gap:16px">
        <label style="display:flex;align-items:center;gap:6px;margin:0">
          <input type="checkbox" id="prnd"> Рандомизировать теги и описание
        </label>
        <button class="b" id="pgo" onclick="publish()">Опубликовать</button>
      </div>
    </div>
    <p class="muted" style="margin-top:12px">
      Без галочки теги и описание берутся детерминированно — как в шаблоне.
      Галочка = перемешать и выкинуть часть, один раз на запуск.
    </p>
  </div>
  <div class="card">
    <h2>Ход работы</h2>
    <div id="log">—</div>
  </div>
</section>

<section class="tab" id="t-set">
  <div class="card">
    <h2>Публикация</h2>
    <div class="g2">
      <div><label>Час (по Омску)</label><input type="number" id="sh" min="0" max="23"></div>
      <div><label>Минута</label><input type="number" id="sm" min="0" max="59"></div>
      <div><label>Горизонт по умолчанию, дней</label><input type="number" id="shz" min="1" max="7"></div>
      <div><label>Часовой пояс</label><input id="stz" style="width:100%"></div>
    </div>
  </div>
  <div class="card">
    <h2>Рендер</h2>
    <div class="g2">
      <div><label>Ширина</label><input type="number" id="rw" min="320"></div>
      <div><label>Высота</label><input type="number" id="rh" min="240"></div>
      <div><label>FPS</label><input type="number" id="rf" min="1" max="60"></div>
      <div><label>CRF (меньше = лучше и больше)</label><input type="number" id="rc" min="0" max="51"></div>
      <div><label>Выход из темноты, сек</label><input type="number" id="rfd" step="0.5" min="0" max="60"></div>
      <div><label>Порог чёрного кадра (0-255)</label><input type="number" id="rbt" min="0" max="255"></div>
      <div><label style="display:flex;align-items:center;gap:6px;margin:0">
        <input type="checkbox" id="rcrop"> Обрезать по центру (иначе чёрные поля)</label></div>
      <div><label style="display:flex;align-items:center;gap:6px;margin:0">
        <input type="checkbox" id="rseam"> Бесшовный цикл видео (палиндром)</label></div>
    </div>
  </div>
  <div class="card">
    <h2>Загрузка</h2>
    <div class="g2">
      <div><label>Попыток</label><input type="number" id="uret" min="1" max="10"></div>
      <div><label>Пауза между попытками, сек</label><input type="number" id="urd" min="1"></div>
      <div><label>Параллельных загрузок</label><input type="number" id="ump" min="1" max="16"></div>
      <div><label style="display:flex;align-items:center;gap:6px;margin:0">
        <input type="checkbox" id="ustream"> Рендер сразу в загрузку (без записи на диск)</label></div>
      <div><label style="display:flex;align-items:center;gap:6px;margin:0">
        <input type="checkbox" id="uthumb"> Поправлять чёрную обложку</label></div>
      <div><label style="display:flex;align-items:center;gap:6px;margin:0">
        <input type="checkbox" id="udel"> Удалять файл после загрузки</label></div>
    </div>
    <div class="row" style="margin-top:14px">
      <button class="b" onclick="saveSet()">Сохранить</button>
      <span class="muted" id="smsg"></span>
    </div>
  </div>
</section>

<section class="tab" id="t-his">
  <div class="card">
    <div class="row"><h2 style="margin:0">Журнал</h2><span class="sp"></span>
      <button class="b g" onclick="loadHis()">Обновить</button>
      <a class="pill" href="/report.html" target="_blank">report.html</a></div>
    <pre id="hsum" class="mono" style="margin-top:12px;white-space:pre-wrap"></pre>
  </div>
  <div class="card" style="padding:0;overflow:hidden">
    <table><thead><tr><th>Когда</th><th>Человек</th><th>Канал</th><th>Заголовок</th>
      <th>Трек</th><th>Дата публикации</th><th>Размер</th><th>Рендер</th><th>Обложка</th><th>Ссылка</th></tr></thead>
      <tbody id="hb"><tr><td colspan="10" class="muted">пусто</td></tr></tbody></table>
  </div>
</section>

<section class="tab" id="t-tok">
  <div class="card">
    <div class="row"><h2 style="margin:0">Токены</h2><span class="sp"></span>
      <button class="b g" onclick="loadTok()">Проверить</button></div>
    <p class="muted" style="margin-top:8px">
      Проверка реально обращается к Google: какой канал за токеном и есть ли право на загрузку.
    </p>
  </div>
  <div class="card" style="padding:0;overflow:hidden">
    <table><thead><tr><th>Папка</th><th>Канал</th><th>Право загрузки</th><th>Состояние</th></tr></thead>
      <tbody id="tb"><tr><td colspan="4" class="muted">—</td></tr></tbody></table>
  </div>
</section>

</main>

<script>
const $=id=>document.getElementById(id);
const esc=s=>String(s==null?'':s).replace(/[&<>"]/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;'}[c]));

document.querySelectorAll('nav button').forEach(b=>b.onclick=()=>{
  document.querySelectorAll('nav button').forEach(x=>x.classList.remove('on'));
  document.querySelectorAll('.tab').forEach(x=>x.classList.remove('on'));
  b.classList.add('on');
  $('t-'+b.dataset.t).classList.add('on');
  if(b.dataset.t==='his')loadHis();
  if(b.dataset.t==='tok')loadTok();
});

let S=null;

async function load(){
  const r=await fetch('/api/state'); S=await r.json();
  if(S.error){ $('chb').innerHTML='<tr><td colspan=10 class=err-t>'+esc(S.error)+'</td></tr>'; return; }

  $('q-ins').textContent='вставки '+S.quota.inserts+'/'+S.quota.insert_limit;
  $('q-gen').textContent='квота '+S.quota.general+'/'+S.quota.general_limit;
  const R=S.run;
  const st=$('st');
  st.textContent = R.running? 'работает' : 'idle';
  st.className = 'badge '+(R.running?'busy':'');
  $('pgo').disabled=!!R.running;
  $('pgo').textContent = R.running? 'идёт…' : 'Опубликовать';
  if(R.log && R.log.length) $('log').textContent=R.log.join('\n');
  if(R.running) setTimeout(load,1500);

  const sel=$('pch'); const cur=sel.value;
  sel.innerHTML='<option value="">все каналы</option>'+
    S.channels.filter(c=>c.has_token).map(c=>'<option value="'+esc(c.folder)+'">'+esc(c.person)+' / '+esc(c.folder)+'</option>').join('');
  sel.value=cur;

  $('chb').innerHTML = S.channels.map(c=>{
    const tok = !c.has_token ? '<span class="pill n">нет</span>'
             : !c.has_upload_scope ? '<span class="pill w">нет права</span>'
             : '<span class="pill y">ок</span>';
    const ready = c.ready? '' : '<span class="pill n">не готов</span>';
    return `<tr>
      <td>${esc(c.person)}</td>
      <td class="mono">${esc(c.folder)}</td>
      <td>${esc(c.title)} ${ready}</td>
      <td><span class="pill">${esc(c.mode)}</span></td>
      <td>${c.music}</td><td>${c.pictures}</td><td>${c.videos}</td>
      <td><div class="bar"><i style="width:${c.music?Math.round(c.used_music/c.music*100):0}%"></i></div>
          <span class="muted">${c.used_music}/${c.music}</span></td>
      <td>${tok}</td>
      <td>${c.enabled?'':'<span class="pill">выкл</span>'}</td>
    </tr>`;
  }).join('') || '<tr><td colspan=10 class=muted>каналов нет</td></tr>';

  fillSet();
}

function fillSet(){
  if(!S||!S.publish) return;
  if(document.activeElement && /INPUT|SELECT/.test(document.activeElement.tagName)) return;
  $('sh').value=S.publish.hour; $('sm').value=S.publish.minute;
  $('shz').value=S.publish.horizon; $('stz').value=S.publish.timezone||'';
  $('phz').value=S.publish.horizon;
}

async function publish(){
  const w=$('pw').value;
  const body={channel:$('pch').value||null, randomize:$('prnd').checked,
              horizon:+$('phz').value||1, workers: w? +w : null};
  $('pgo').disabled=true;
  const r=await fetch('/api/publish',{method:'POST',headers:{'Content-Type':'application/json'},
    body:JSON.stringify(body)});
  const j=await r.json();
  if(j.error){ alert(j.error); $('pgo').disabled=false; return; }
  load();
}

async function saveSet(){
  const body={
    render:{width:+$('rw').value,height:+$('rh').value,fps:+$('rf').value,crf:+$('rc').value,
      preset:'veryfast',audio_bitrate:'160k',audio_rate_hz:44100,
      fade_from_black_secs:+$('rfd').value,seamless_loop:$('rseam').checked,
      crop_to_fill:$('rcrop').checked,black_frame_threshold:+$('rbt').value,thumb_at_secs:null},
    publish:{hour:+$('sh').value,minute:+$('sm').value,timezone:$('stz').value||null,
      workers:null,upload_retries:+$('uret').value,retry_base_delay_secs:+$('urd').value,
      fix_black_thumbnail:$('uthumb').checked,delete_after_upload:$('udel').checked,
      horizon_days:+$('shz').value,stream_to_upload:$('ustream').checked},
    api:{insert_bucket_per_day:100,general_units_per_day:10000,quota_warn_percent:85,
      max_parallel_uploads:+$('ump').value},
    log_level:null
  };
  const r=await fetch('/api/settings',{method:'PUT',headers:{'Content-Type':'application/json'},
    body:JSON.stringify(body)});
  const j=await r.json();
  $('smsg').textContent = j.error? ('ошибка: '+j.error) : 'сохранено. Перезапусти ytf web.';
  $('smsg').className = j.error? 'err-t':'muted';
}

async function loadHis(){
  const r=await fetch('/api/history'); const j=await r.json();
  $('hsum').textContent=j.summary||'';
  $('hb').innerHTML=(j.entries||[]).map(e=>`<tr>
    <td class="mono">${esc(e.at)}</td><td>${esc(e.person)}</td><td>${esc(e.channel)}</td>
    <td>${esc(e.title)}</td><td class="mono">${esc(e.track)}</td>
    <td class="mono">${esc((e.publish_at||'').slice(0,16))}</td>
    <td>${(+e.size_mb).toFixed(1)}</td><td>${(+e.render_secs).toFixed(0)}с</td>
    <td>${e.thumbnail_fixed?'<span class="pill y">поправлена</span>':''}${
      e.error?'<span class="pill n">ошибка</span>':''}</td>
    <td>${e.url?`<a href="${esc(e.url)}" target="_blank">открыть</a>`:''}</td></tr>`).join('')
    || '<tr><td colspan=10 class=muted>пусто</td></tr>';
}

async function loadTok(){
  $('tb').innerHTML='<tr><td colspan=4 class=muted>проверяю…</td></tr>';
  const r=await fetch('/api/tokens'); const j=await r.json();
  $('tb').innerHTML=(j.tokens||[]).map(t=>`<tr>
    <td class="mono">${esc(t.folder)}</td><td>${esc(t.info||t.title)}</td>
    <td>${t.has_upload_scope?'<span class="pill y">есть</span>':'<span class="pill n">нет</span>'}</td>
    <td>${t.ok?'<span class="pill y">живой</span>':`<span class="pill n">${esc(t.error||'мёртв')}</span>`}</td>
  </tr>`).join('') || '<tr><td colspan=4 class=muted>токенов нет</td></tr>';
}

load();
</script>
</body></html>"##;
