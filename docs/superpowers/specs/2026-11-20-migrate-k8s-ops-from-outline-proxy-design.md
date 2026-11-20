# Спецификация: Перенос Kubernetes инфраструктуры и операций из outline-proxy в ambar

**Дата:** 2026-11-20  
**Статус:** Утверждено  
**Цель:** Консолидировать всю инфраструктуру развертывания k3s кластера (NanoPi R5C), манифесты приложений, мониторинг и документацию в репозитории `ambar`, удалив их из проекта `outline-proxy`.

---

## 1. Контекст и мотивация

Исторически репозиторий `outline-proxy` содержал как сетевой proxy-стек, так и общие файлы эксплуатации домашней инфраструктуры (`ops/`):
- Развертывание и сопровождение физического кластера k3s из 3x NanoPi R5C.
- Декларации приложений Kubernetes (namespace `home`, `smarthome`, `ingress`, `monitoring`, `storage`, `registry`, `vpn`).
- Конфигурации мониторинга и алертинга (vmalert, alertmanager, grafana, nas-monitoring).
- Регулярные задания кластера (registry-gc-cronjob).

Проект `ambar` разрабатывается как нативный Kubernetes CSI-драйвер поверх DRBD 9 именно для этого кластера NanoPi R5C. Хранение инфраструктуры K3s и манифестов кластера в `ambar` логически объединяет уровень блочного хранилища и прикладную нагрузку кластера в одном месте, освобождая `outline-proxy` от несвязанного инфраструктурного кода.

---

## 2. Объем и структура переноса

### 2.1. Каталог `ops/` в `ambar`
В корень репозитория `ambar` переносятся:
1. **`ops/nanopi-r5c-k3s/`**:
   - `README.md` — исчерпывающее руководство по железу NanoPi R5C, разбиению eMMC/NVMe, установке Debian johang и настройке k3s.
   - `apps/` — все рабочие манифесты приложений:
     - `namespaces.yaml` — декларация пространств имен.
     - `deploy.sh` — главный скрипт выкатки приложений в кластер.
     - `home/` — `jellyfin.yaml`, `mosquitto.yaml`, `zigbee2mqtt.yaml`.
     - `smarthome/` — `conditioner.yaml`, `humidity.yaml`, `power.yaml`, `presence.yaml`, `samsung-tv.yaml`, `README.md`.
     - `ingress/` — `metallb-pool.yaml`, `traefik.values.yaml`, `tls-store.yaml`, `ingress-routes.yaml`, `cert-publisher.rbac.yaml`, `publish-cert.sh`, `README.md`.
     - `monitoring/` — `alertmanager.yaml`, `vmalert.yaml`, `node-exporter.yaml`, `outline-ui.yaml`, `grafana-nas.yaml`, `victoria-metrics-nas.yaml`.
     - `registry/` — `registry.yaml`, `README.md`.
     - `storage/` — `nfs-provisioner.values.yaml`.
     - `vpn/` — `ocserv.yaml`, `ocserv-certs.secret.example.yaml`.
     - `descheduler/` — `descheduler.values.yaml`.
     - `outline/` — манифесты `outline-ss-rust.daemonset.yaml`, `outline-ws-rust.deployment.yaml` для запуска в k3s.
2. **`ops/k3s/`**:
   - `registry-gc-cronjob.yaml` — CronJob сборки мусора в локальном OCI registry.
3. **`ops/alerting/` и `ops/grafana/`**:
   - `ops/alerting/` — `vmalert/rules.yaml`, `vmalert/tests/`, `alertmanager/alertmanager.yml`, `deploy.sh`, `test.sh`.
   - `ops/grafana/` — правила алертинга `policies.yaml`, `rules.yaml`, `contact-points.yaml`, дашборды и provisioning.
4. **`ops/nas-monitoring/` и `ops/nas-jellyfin-backup/`**:
   - Конфигурации мониторинга на NAS (docker-compose, scrape-конфиги VictoriaMetrics, Grafana datasources, бэкапы).

### 2.2. Документация и спецификации (`docs/superpowers/`)
Перенос планов и спецификаций, описывающих миграции сервисов в k3s:
- Plans:
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
- Specs:
  - `2026-08-09-grafana-to-k3s-design.md`
  - `2026-08-09-k3s-tls-beerloga-design.md`
  - `2026-08-09-mosquitto-to-k3s-design.md`
  - `2026-08-09-samsung-tv-tokens-per-ip-design.md`
  - `2026-08-09-smarthome-to-k3s-design.md`
  - `2026-08-09-victoriametrics-to-k3s-design.md`
  - `2026-08-09-zigbee2mqtt-to-k3s-design.md`

### 2.3. Синхронизация репозиториев
1. В `ambar`:
   - Создаются каталоги `ops/` и переносятся файлы.
   - Добавляется `FLEET.local.md` в `.gitignore` `ambar`.
   - Затрагиваемые скрипты и документация верифицируются.
   - Коммит и пуш в `ambar`.
2. В `outline-proxy`:
   - Удаление перенесенных файлов через `git rm -r`.
   - Коммит и пуш в `outline-proxy`.
