# ytf — автозаливка type beat на YouTube

Программа сама: рендерит видео (1080p, фон + звук) → заливает на канал →
ставит отложенную публикацию → проверяет, что YouTube принял файл →
ставит свою обложку, если YouTube выбрал чёрный кадр → удаляет временные файлы.

Работает с каналами, которые заведены в **Nstbrowser** (все профили видны,
важно лишь открыть ссылку авторизации в профиле нужного канала).

---

## 1. Установка (Mac) — 5 минут

1. Распакуй архив в любое место, например в `~/ytf`:
   ```bash
   cd ~/Downloads
   unzip ytf-pack.zip -d ~/ytf
   cd ~/ytf
   ```
2. Сними карантин с бинарника (macOS блокирует всё, что не из App Store):
   ```bash
   xattr -dr com.apple.quarantine .
   chmod +x ytf-macos
   ```
3. Проверь, что всё на месте:
   ```bash
   ./ytf-macos doctor
   ```
   Должно быть `ffmpeg: OK`, `каналов: 1 (шаблон)`.

Если ругается на ffmpeg — он уже лежит в папке `tools/ffmpeg/`, ничего
ставить не надо. На Apple Silicon при первом запуске macOS может
попросить разрешить Rosetta — соглашайся.

## 2. Подготовь Google-проект (один раз, 10 минут)

Это самый муторный шаг, но делается один раз на всех каналов.

1. Зайди на https://console.cloud.google.com под СВОИМ Google-аккаунтом.
2. Создай проект (любое имя, например `ytf-pipeline`).
3. В поиске консоли найди и включи две API:
   - **YouTube Data API v3** (Enable)
   - **YouTube Analytics API** (Enable, для статистики)
4. Слева в меню: **APIs & Services → OAuth consent screen** (Google Auth Platform):
   - Audience: User type **External**
   - Branding: App name — любое, support email — твоя почта,
     **Application home page** — любой твой сайт (можно GitHub Pages),
     **Application privacy policy** — страница на том же сайте,
     **Authorized domains** — домен этого сайта (например `github.io`)
   - Developer contact — твоя почта
   - Вкладка **Audience** → **Publish app** (без этого токен живёт 7 дней!)
5. **APIs & Services → Credentials → Create credentials → OAuth client ID**:
   - Application type: **Desktop app**
   - Create → кнопка **Download JSON** → сохрани файл.
6. Положи скачанный файл в папку программы под именем
   `client_secret.json` (рядом с `settings.json`):
   ```bash
   mv ~/Downloads/client_secret_xxxx.json ~/ytf/client_secret.json
   ```

> **Важно про «Publish app»**: Google с 2026 года требует home page +
> privacy policy, иначе кнопка Publish серая. Бесплатное решение —
> GitHub Pages: создаёшь репозиторий, заливаешь два html-файла
> (есть в архиве: `site/index.html` и `site/privacy.html`),
> включаешь Pages — получаешь `https://логин.github.io/репо/`.

## 3. Подготовь папку канала

Канал — это папка внутри `People/<человек>/<имя канала>/`.
Готовый шаблон лежит в `channel-template` — скопируй и наполни:

```
People/my/channel-1/
├── music/                  треки (mp3/wav/flac)
│   └── BABY 104BPM D MIN.mp3
├── pictures/               фон: картинки (jpg/png) ИЛИ mp4-видео (для loop-режима)
│   └──IMG_0001.jpg
├── names/                  варианты заголовков, по одному в строке
│   └── names.txt           [FREE] Genre Type Beat - NAMETRACK
├── description/
│   └── description.txt     описание с подстановками:
│                           NAME_XXX → имя бита, BPM_XXX → bpm, KEY_XXX → тональность
├── tags_for_video/         теги видео через запятую
│   └── tagsv1.txt
└── tags_for_description/   теги в конец описания (через запятую)
    └── tagsd1.txt
```

Имя трека — источник метаданных: `BABY 104BPM D MIN BOUNCE.mp3` →
бит «BABY», bpm «104 BPM», тональность «D MIN» (понимаются форматы
`D MIN`, `Gmin`, `Cminor`, `A#minor`, `G Maj`).

Проверь, что канал виден:
```bash
./ytf-macos library
```

## 4. Получи токен канала — ГЛАВНЫЙ ШАГ

Токен = разрешение программе заливать видео на ЭТОТ канал.
Один канал = один запуск команды.

1. Открой в Nstbrowser профиль нужного канала (YouTube должен быть залогинен).
2. В обычном терминале (Terminal.app):
   ```bash
   cd ~/ytf
   ./ytf-macos auth "channel-1" --title "Название канала на YouTube"
   ```
   (вместо `channel-1` — имя папки из People/my/)
3. Программа напечатает ссылку Google и **сама её откроет**.
   Если открылась не в том браузере — скопируй ссылку из терминала
   и открой её **во вкладке профиля канала в Nstbrowser**.
4. Нажми **«Продолжить» / «Allow»** (предупреждение «непроверенное
   приложение»: «Дополнительные настройки» → «Перейти на страницу...»).
5. Всё. Браузер покажет «✓ Токен получен», программа сама запишет токен
   и сама проверит канал:
   ```
   [OK] channel-1 -> Название канала на YouTube
   ```

Никаких кодов копировать не нужно — программа ловит ответ сама.

## 5. Публикация

```bash
./ytf-macos publish "channel-1"
```

Что произойдёт: возьмёт свободный слот (сегодня, если свободно, иначе
завтра), время публикации — 22:00 (настраивается), отрендерит, зальёт,
дождётся подтверждения YouTube, поправит чёрную обложку и запишет всё
в `history.json`. Видео появится на канале в назначенное время.

Публикация сразу по всем каналам:
```bash
./ytf-macos publish
```

Проверка токенов/каналов в любой момент:
```bash
./ytf-macos tokens
./ytf-macos library
```

## 6. Веб-панель (опционально)

```bash
./ytf-macos web --port 3000
```
Открой `http://localhost:3000` — кнопки «Опубликовать», настройки времени,
история, квота.

---

## Настройки (settings.json)

| Поле | Что значит |
|---|---|
| `publish.hour/minute` | время публикации (22:00 по умолчанию) |
| `publish.timezone` | `Asia/Omsk` — поменяй под свой город |
| `publish.horizon_days` | на сколько дней вперёд планировать |
| `publish.delete_after_upload` | удалять локальные файлы после загрузки |
| `render.fade_from_black_secs` | «выход из темноты», сек (0 = выкл) |
| `render.seamless_loop` | зацикливание фона палиндромом (видеофон) |

## Возможные проблемы

- **«invalid_grant» в tokens** — токен протух (было в Testing-режиме) или
  приложение не опубликовано. Повтори `auth` (шаг 4).
- **«суточный лимит загрузок»** — YouTube ограничивает число заливок на
  канал в день (~10 для новых). Придёт в себя за сутки.
- **«не обработано YouTube»** — бот сам проверяет: если файл не принят,
  это ошибка. Просто перезапусти publish.
- **Google просит «Verify it's you»** при входе в Studio — это нормально,
  проходи проверку, на загрузки через API не влияет.
- Кнопка Publish app серая — не заполнены home page + privacy policy
  (см. шаг 2).

## Что внутри архива

```
ytf-macos            программа (Mac, Apple Silicon/Intel)
ytf.exe              программа (Windows, запасная)
settings.json        настройки
client_secret.json   ← сюда положи файл от Google (шаг 2)
People/my/channel-template/   шаблон канала
site/                home page + privacy policy для Google (GitHub Pages)
tools/ffmpeg/        ffmpeg — ставить ничего не нужно
start-mac.command    двойной клик — откроет терминал с программой
```

## Коротко: с нуля до первого видео

```bash
unzip ytf-pack.zip -d ~/ytf && cd ~/ytf
xattr -dr com.apple.quarantine . && chmod +x ytf-macos
# положи client_secret.json, наполни People/my/channel-1, затем:
./ytf-macos doctor
./ytf-macos auth "channel-1" --title "Мой канал"
./ytf-macos publish "channel-1"
```
