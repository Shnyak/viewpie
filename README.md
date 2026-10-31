# viewpie (fork with HTTP API)

> **⚠️ Это форк оригинального проекта**
>
> **Оригинал:** [snoack/viewpie](https://github.com/snoack/viewpie)
> **Автор оригинала:** Sebastian Noack
> **Лицензия оригинала:** GPL-3.0-or-later

## Авторство модификаций

Все изменения (HTTP REST API, поддержка панелей, динамическое конфигурирование)
были **сгенерированы с помощью AI-ассистента Qwen (Alibaba Cloud)** по
техническому заданию пользователя.

Код предоставляется "как есть", без каких-либо гарантий. Перед использованием
в production требуется тщательное ревью и тестирование.
См. также [AI_GENERATED.md](AI_GENERATED.md) и [SECURITY.md](SECURITY.md).

## Отличия от оригинала

- Встроенный HTTP REST API на axum (порт 8080) для интеграции с Home Assistant
- Поддержка нескольких панелей (сцен) с graceful-переключением
- Динамическое изменение `spacing` / `pause_off_screen` / `background` /
  `rotation_interval` / `fullscreen_timeout` без перезапуска
- Reference counting RTSP-декодеров: общие камеры не переподключаются при
  переключении панелей
- Автостоп декодеров при выключенном мониторе (DPMS через sysfs — работает без CEC)
- Полная совместимость с оригинальным управлением по HDMI-CEC

## HTTP API

Базовый URL: `http://<rpi-ip>:8080`. Все ответы — JSON.

### Управление

| Метод | Путь | Описание |
|---|---|---|
| POST | `/api/pause` | Пауза: остановить все декодеры, чёрный экран |
| POST | `/api/resume` | Возобновить активную панель (сетка или fullscreen) |
| POST | `/api/grid` | Выйти из fullscreen в сетку |
| POST | `/api/fullscreen` | `{"camera":"pool","duration_seconds":60}` — переход на main-поток |
| POST | `/api/reload` | Перечитать `/etc/viewpie/viewpie.yaml` без рестарта |

### Настройки отображения

| Метод | Путь | Описание |
|---|---|---|
| GET | `/api/settings` | Текущие DisplaySettings |
| PUT | `/api/settings` | Частичное обновление (`spacing`, `background`, `pause_off_screen`, `rotation_interval`, `fullscreen_timeout`) — применяется на лету |

### Панели

| Метод | Путь | Описание |
|---|---|---|
| GET | `/api/panels` | Список панелей + активная |
| GET | `/api/panels/active` | Активная панель |
| GET | `/api/panels/{id}` | Полная структура панели |
| POST | `/api/panels` | Создать панель (валидация: уникальный id, квадратная сетка 1×1–8×8, существующие камеры) |
| PUT | `/api/panels/{id}` | Обновить (активная — немедленно) |
| DELETE | `/api/panels/{id}` | Удалить (нельзя удалить последнюю) |
| POST | `/api/panels/{id}/activate` | Переключить панель (diff ref_count + atomic KMS commit) |

### Камеры

| Метод | Путь | Описание |
|---|---|---|
| GET | `/api/cameras` | Список камер с состоянием декодеров/ref_count |
| POST | `/api/cameras` | Добавить (ref_count = 0, декодер стартует при первом использовании) |
| PUT | `/api/cameras/{name}` | Обновить URL (при ref_count > 0 — перезапуск декодера) |
| DELETE | `/api/cameras/{name}` | Удалить; `409 Conflict`, если камера используется панелью |
| POST | `/api/cameras/{name}/test` | RTSP OPTIONS-проверка доступности |

### Статус

| Метод | Путь | Описание |
|---|---|---|
| GET | `/api/status` | paused/screen_off/active_panel/uptime/cpu_temp_c/active_streams и т.д. |

Пример интеграции с Home Assistant:

```yaml
rest_command:
  viewpie_pause:
    url: http://10.0.0.20:8080/api/pause
    method: POST
  viewpie_show_outside:
    url: http://10.0.0.20:8080/api/panels/outside/activate
    method: POST
  viewpie_fullscreen_gate:
    url: http://10.0.0.20:8080/api/fullscreen
    method: POST
    headers:
      content-type: application/json
    body: '{"camera": "gate", "duration_seconds": 60}'
```

## Конфигурация

Формат YAML (см. [examples/viewpie.example.yaml](examples/viewpie.example.yaml)):
секции `display`, `feeds` (с опциональным `fullscreen_url`), `panels`
(`layout` + `viewports`) и `defaults`. Стандартный путь:
`/etc/viewpie/viewpie.yaml`.

## Сборка и установка

### Нативная сборка (на Raspberry Pi)

```bash
sudo apt install libavformat-dev libavcodec-dev libswscale-dev \
    libavutil-dev libdrm-dev pkg-config libcec-dev
cargo build --release --all-features
sudo cp target/release/viewpie /usr/bin/
```

Без аппаратного вывода (только API, для разработки):
`cargo build --release --no-default-features`.

### .deb пакет (armhf)

```bash
./scripts/build-deb.sh        # cross-compile через Dockerfile.build
sudo dpkg -i target/debian/viewpie_*_armhf.deb
systemctl enable --now viewpie
```

### Docker

```bash
docker build -t viewpie .     # Dockerfile (runtime)
docker run --network host --device /dev/dri/card0 \
    -v /etc/viewpie:/etc/viewpie:ro viewpie
```

Готовый `compose.yaml` включён в репозиторий.

## Тесты

```bash
cargo test --all-features          # unit + integration (axum Router)
cargo clippy --all-features -- -D warnings
cargo fmt --all -- --check
```

## Лицензия

GPL-3.0-or-later (унаследована от оригинала). Оригинальный код —
Copyright (c) Sebastian Noack.
