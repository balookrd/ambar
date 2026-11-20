# План реализации: Перенос Kubernetes инфраструктуры и операций из outline-proxy в ambar

> **Для исполнителей:** ОБЯЗАТЕЛЬНЫЙ ПОД-НАВЫК: Используйте `superpowers:executing-plans` для последовательного выполнения задач по шагам с чекбоксами (`- [ ]`).

**Цель:** Перенести каталоги `ops/` и связанную документацию K3s из `outline-proxy` в `ambar`, обновить конфигурации и удалить перенесенные файлы из `outline-proxy`.

**Архитектура:** Файлы кластера k3s (NanoPi R5C, apps, alerting, grafana, nas-monitoring, nas-jellyfin-backup, k3s cronjob) размещаются в `ambar/ops/` с сохранением внутренней иерархии для поддержки скриптов развертывания (`deploy.sh`). Исторические планы и спецификации K3s переносятся в `ambar/docs/superpowers/`.

---

### Задача 1: Перенос каталогов `ops/` в `ambar`

**Файлы:**
- Создать в `ambar`: `ops/nanopi-r5c-k3s/`, `ops/k3s/`, `ops/alerting/`, `ops/grafana/`, `ops/nas-monitoring/`, `ops/nas-jellyfin-backup/`
- Скопировать из `../outline-proxy/ops/`
- Модифицировать в `ambar`: `.gitignore` (добавить `FLEET.local.md`, если потребуется)

- [ ] **Шаг 1: Скопировать `ops/nanopi-r5c-k3s` из outline-proxy в ambar**
- [ ] **Шаг 2: Скопировать `ops/k3s` из outline-proxy в ambar**
- [ ] **Шаг 3: Скопировать `ops/alerting` и `ops/grafana` из outline-proxy в ambar**
- [ ] **Шаг 4: Скопировать `ops/nas-monitoring` и `ops/nas-jellyfin-backup` из outline-proxy в ambar**
- [ ] **Шаг 5: Добавить `FLEET.local.md` в `.gitignore` репозитория `ambar` и скопировать его (если существует)**
- [ ] **Шаг 6: Проверить корректность прав исполнения (`chmod +x`) на скриптах `deploy.sh`, `publish-cert.sh`**

---

### Задача 2: Перенос документации и спецификаций k3s в `ambar`

**Файлы:**
- Скопировать в `ambar/docs/superpowers/plans/`:
  - `2026-08-09-grafana-to-k3s.md`
  - `2026-08-09-k3s-tls-beerloga.md`
  - `2026-08-09-mosquitto-to-k3s.md`
  - `2026-08-09-samsung-tv-tokens-per-ip.md`
  - `2026-08-09-smarthome-to-k3s.md`
  - `2026-08-09-victoriametrics-to-k3s.md`
  - `2026-08-09-zigbee2mqtt-to-k3s.md`
  - `2026-11-08-alerting-vmalert-alertmanager.md`
  - `2026-11-08-k3s-nfs-125-to-105.md`
  - `2026-11-09-grafana-vm-to-nas.md`
  - `2026-11-19-nas-node-exporter.md`
- Скопировать в `ambar/docs/superpowers/specs/`:
  - `2026-08-09-grafana-to-k3s-design.md`
  - `2026-08-09-k3s-tls-beerloga-design.md`
  - `2026-08-09-mosquitto-to-k3s-design.md`
  - `2026-08-09-samsung-tv-tokens-per-ip-design.md`
  - `2026-08-09-smarthome-to-k3s-design.md`
  - `2026-08-09-victoriametrics-to-k3s-design.md`
  - `2026-08-09-zigbee2mqtt-to-k3s-design.md`

- [ ] **Шаг 1: Скопировать планы и спецификации из outline-proxy в ambar**
- [ ] **Шаг 2: Проверить целостность перенесенных файлов**

---

### Задача 3: Верификация и коммит в `ambar`

- [ ] **Шаг 1: Проверить `git status` в `ambar`**
- [ ] **Шаг 2: Запустить тесты `cargo fmt --check && cargo test` для подтверждения, что кодовая база не затронута**
- [ ] **Шаг 3: Закоммитить и запушить изменения в `ambar` (`main`)**

---

### Задача 4: Удаление перенесенных файлов из `outline-proxy` и фиксация изменений

- [ ] **Шаг 1: Выполнить `git rm -r` в `outline-proxy` для перенесенных каталогов `ops/` и `docs/superpowers/`**
- [ ] **Шаг 2: Проверить `git status` и сборку/тесты в `outline-proxy` (`cargo test`)**
- [ ] **Шаг 3: Закоммитить и запушить удаление в `outline-proxy`**
