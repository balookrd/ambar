# Установка node-exporter на NAS UGREEN (.105): план реализации

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Цель:** Развернуть `node-exporter` на NAS UGREEN `198.18.1.105` в рамках существующего compose-проекта `monitoring` и подключить сбор метрик в VictoriaMetrics под именем инстанса `ugreen` без простоя существующих сервисов (VictoriaMetrics и Grafana).

**Архитектура:** Контейнер `node-exporter` (версия 1.11.1, закреплённая по digest sha256) добавляется в `/volume1/docker/monitoring/docker-compose.yaml`. Он использует `pid: host` и `network_mode: host` с монтированием хостового корня в `/host:ro,rslave` и исключением оверлейных и системных точек монтирования UGOS. VictoriaMetrics скрейпит `198.18.1.105:9100` в джобе `node-exporter` с меткой `hostname: 'ugreen'`, которая relabel-правилом превращается в `instance="ugreen"`.

**Стек:** Docker 29.6.2 + Compose v5.1.3 на UGOS (Debian 12 bookworm x86_64, ядро 6.18), `prom/node-exporter:v1.11.1`, VictoriaMetrics v1.155.0.

## Глобальные ограничения

- Образ `prom/node-exporter` той же версии, что на остальном парке (.102, .104, k3s):
  `prom/node-exporter@sha256:e9cff4fc67b1818f8c97adb115b9f12c9a54b533de86765d4a0effc01b357205` (v1.11.1, multi-arch amd64/arm64).
- Сеть `network_mode: host` и порт 9100: экспортёр слушает напрямую на хосте `198.18.1.105:9100` (порт свободен, проверено).
- Метка хоста строго `hostname: 'ugreen'` (по relabel_configs становится `instance="ugreen"`).
- Исключение служебных файловых систем UGOS и Docker через `--collector.filesystem.mount-points-exclude`: исключаются `/dev`, `/proc`, `/sys`, `/run`, `/rootfs`, `/mnt/factory`, `/volume1/@docker`.
- Никакого простоя VictoriaMetrics и Grafana: поднимается только целевой контейнер через `docker compose up -d node-exporter`, а конфигурация скрейпа VictoriaMetrics (`scrape.yaml`) подхватывается автоматически раз в 60 с без рестарта.
- Документация в `ops/nas-monitoring/README.md` обновляется в рамках той же задачи.

---

### Task 1: Конфигурация compose и скрейпа в репозитории

**Файлы:**
- Модифицировать: `ops/nas-monitoring/docker-compose.yaml`
- Модифицировать: `ops/nas-monitoring/victoria-metrics/scrape.yaml`
- Модифицировать: `ops/nas-monitoring/README.md`

- [x] **Шаг 1: Добавить сервис `node-exporter` в `ops/nas-monitoring/docker-compose.yaml`**

```yaml
  node-exporter:
    image: prom/node-exporter@sha256:e9cff4fc67b1818f8c97adb115b9f12c9a54b533de86765d4a0effc01b357205 # v1.11.1
    container_name: node-exporter
    pid: host
    network_mode: host
    command:
      - --path.rootfs=/host
      - --path.procfs=/host/proc
      - --path.sysfs=/host/sys
      - --collector.filesystem.mount-points-exclude=^/(dev|proc|sys|run|rootfs|mnt/factory|volume1/@docker)($|/)
    volumes:
      - /:/host:ro,rslave
    mem_limit: 128m
    restart: unless-stopped
```

- [x] **Шаг 2: Добавить таргет `ugreen` в `ops/nas-monitoring/victoria-metrics/scrape.yaml`**

В секцию `job_name: node-exporter` добавить:
```yaml
  - targets:
    - 198.18.1.105:9100
    labels:
      hostname: 'ugreen'
```

- [x] **Шаг 3: Обновить документацию в `ops/nas-monitoring/README.md`**

Добавить `node-exporter` в сводную таблицу контейнеров и описание состава проекта.

- [x] **Шаг 4: Проверить валидность docker-compose конфигурации локально**

Выполнить:
```bash
docker compose -f ops/nas-monitoring/docker-compose.yaml config -q
```
Ожидается: успешное завершение (код 0, без ошибок).

---

### Task 2: Выкатка и запуск на боевом NAS `.105`

**Файлы:**
- Хост: `sysadm@198.18.1.105:/volume1/docker/monitoring/`

- [x] **Шаг 1: Скопировать обновлённую конфигурацию на NAS**

Выполнить:
```bash
./ops/nas-monitoring/deploy.sh
```
Ожидается: успешное копирование `docker-compose.yaml` и `scrape.yaml` на NAS в `/volume1/docker/monitoring/`.

- [x] **Шаг 2: Запустить контейнер `node-exporter` на NAS**

Выполнить:
```bash
ssh sysadm@198.18.1.105 "cd /volume1/docker/monitoring && docker compose up -d node-exporter && docker compose ps"
```
Ожидается:
- Контейнер `node-exporter` создан и перешёл в статус `Up`.
- Существующие контейнеры `victoria-metrics`, `grafana`, `vmbackup`, `grafana-backup` остаются `Up` без перезапуска.

---

### Task 3: Верификация сбора метрик и здоровья сервисов

- [x] **Шаг 1: Проверить отдачу метрик с `node-exporter` на порту 9100**

Выполнить:
```bash
curl -s http://198.18.1.105:9100/metrics | head -n 25
```
Ожидается: вывод метрик prometheus (комментарии `# HELP ...`, `# TYPE ...`).

- [x] **Шаг 2: Проверить статус скрейп-цели в VictoriaMetrics**

Выполнить (дождавшись интервала проверки scrape 60 с):
```bash
curl -s http://198.18.1.105:8428/api/v1/targets | grep -B 2 -A 5 "198.18.1.105:9100"
```
Ожидается: `health: "up"`, метки `job="node-exporter"`, `instance="ugreen"`.

- [x] **Шаг 3: Проверить запрос метрик по инстансу `ugreen` в VictoriaMetrics**

Выполнить:
```bash
curl -s "http://198.18.1.105:8428/api/v1/query?query=up%7Bjob%3D%22node-exporter%22%2Cinstance%3D%22ugreen%22%7D"
```
Ожидается: JSON-ответ со статусом `"success"` и значением `1`.

- [x] **Шаг 4: Проверить сбор ключевых метрик ОС (CPU, память, диски)**

Выполнить:
```bash
curl -s "http://198.18.1.105:8428/api/v1/query?query=node_filesystem_size_bytes%7Binstance%3D%22ugreen%22%2Cmountpoint%3D%22%2Fhost%2Fvolume1%22%7D"
```
Ожидается: размер файловой системы `/volume1` (~7.3-8 TB).

---

### Task 4: Актуализация памяти и CI-проверки

- [x] **Шаг 1: Прогнать локальные тесты / monorepo проверки**

```bash
git diff --check
```

- [x] **Шаг 2: Актуализировать файл памяти Claude**

Обновить `~/.claude/projects/-Users-mvmalykh-IdeaProjects-outline-proxy/memory/vm-move-to-105-planned.md` (или `MEMORY.md`), зафиксировав наличие `node-exporter` (`instance="ugreen"`) на `.105`.
