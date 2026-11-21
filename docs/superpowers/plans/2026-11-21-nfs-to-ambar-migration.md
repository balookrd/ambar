# План реализации миграции рабочих томов с NFS на `ambar-r3-retain`

> **Для агентов-исполнителей:** ОБЯЗАТЕЛЬНЫЙ ПОД-СКИЛЛ: Используйте `superpowers:subagent-driven-development` или `superpowers:executing-plans` для пошаговой реализации плана. Шаги отслеживаются через чекбоксы (`- [ ]`).

**Цель:** поэтапно (в три волны) перенести постоянные данные сервисов кластера NanoPi R5C (`smarthome`, `home`, `registry`, шаблон `vpn`) с inline-томов NFS (`198.18.1.105:/volume1/k8s/*`) на реплицируемое блочное хранилище `ambar-r3-retain` с сохранением исходных каталогов на NAS в качестве холодного отката.

**Архитектура:** для каждого сервиса создаётся собственный PVC в `StorageClass: ambar-r3-retain` (3 копии, Protocol C, `reclaimPolicy: Retain`). Во время короткого окна обслуживания сервис масштабируется в `0`, данные копируются одноразовым подом-мигратором (`busybox:1.36`) с монтированием NFS в `readOnly: true` и побайтовой сверкой `sha256sum`, после чего применяется обновлённый манифест. Для `smarthome/*` добавляется `fsGroup: 1000`. Для `registry` + `registry-gc` (Волна 3) CronJob привязывается к узлу `registry` через `podAffinity` (совместный RWO на одной ноде проверен в `ambar-verify`).

**Стек технологий:** Kubernetes (`k3s`), `ambar` CSI (`DRBD 9.3.4` + LVM2 `k3s-vg`), NFSv3 (источник `ro`), VictoriaMetrics, Grafana.

## Глобальные ограничения

- Исходные каталоги на NAS (`/volume1/k8s/*`) монтируются строго `readOnly: true` и не удаляются.
- Волны 2 и 3 выполняются строго отдельными разрешениями владельца после обкатки предыдущей волны.
- В публичных документах (`ops/nanopi-r5c-k3s/apps/README.md`, `smarthome/README.md`, `registry/README.md`, `ops/k3s/README*.md`) используются только условные адреса `198.51.100.*` и `fleet.example`.

---

## Волна 1 — Канареечный перенос (`conditioner`, `presence`, `mosquitto`)

### Шаг 1. Подготовка плана и локальных манифестов Волны 1
- [x] Создать файл плана `docs/superpowers/plans/2026-11-21-nfs-to-ambar-migration.md`.
- [x] Обновить `ops/nanopi-r5c-k3s/apps/smarthome/conditioner.yaml`: добавить `fsGroup: 1000`, заменить `nfs:` на `persistentVolumeClaim: conditioner-conf`, добавить `PersistentVolumeClaim/conditioner-conf` (`256Mi`, `ambar-r3-retain`).
- [x] Обновить `ops/nanopi-r5c-k3s/apps/smarthome/presence.yaml`: добавить `fsGroup: 1000`, заменить `nfs:` на `persistentVolumeClaim: presence-conf`, добавить `PersistentVolumeClaim/presence-conf` (`256Mi`, `ambar-r3-retain`).
- [x] Обновить `ops/nanopi-r5c-k3s/apps/home/mosquitto.yaml`: заменить `nfs:` на `persistentVolumeClaim: mosquitto-data`, добавить `PersistentVolumeClaim/mosquitto-data` (`512Mi`, `ambar-r3-retain`).
- [x] Проверить обновлённые манифесты через `kubectl apply --dry-run=client`.

### Шаг 2. Последовательный перенос данных и переключение сервисов
- [x] **2.1. `smarthome/conditioner`**: создать PVC `conditioner-conf`, остановить `deploy/conditioner`, скопировать данные через под-мигратор со сверкой `sha256sum` (`VERIFY_OK`), удалить мигратор, применить `conditioner.yaml`, проверить логи, `devices.json` (`1000:1000`) и `/tmp/conditioner.heartbeat`.
- [x] **2.2. `smarthome/presence`**: создать PVC `presence-conf`, остановить `deploy/presence`, скопировать данные через под-мигратор со сверкой `sha256sum` (`VERIFY_OK`), удалить мигратор, применить `presence.yaml`, проверить логи, `devices.json` (`1000:1000`) и `/tmp/presence.heartbeat`.
- [x] **2.3. `home/mosquitto`**: создать PVC `mosquitto-data`, остановить `deploy/mosquitto`, скопировать `mosquitto.db` через под-мигратор (установить права `chmod 600 /dst/mosquitto.db`, владелец `1000:1000`, сверка `sha256sum` и `VERIFY_OK`), удалить мигратор, применить `mosquitto.yaml`, проверить логи, права `mosquitto.db`, переподключение клиентов (`zigbee2mqtt` и `smarthome/*`) и доступность `198.18.1.201:1883`.

### Шаг 3. Проверка `ambar` и мониторинга после Волны 1
- [x] Подтвердить, что все 3 новых PVC находятся в `Bound` (`ambar-r3-retain`), а все 3 `ReplicatedVolume` — в `3/3 UpToDate`, `quorum: true`, `Connected`.
- [x] Проверить метрики в VictoriaMetrics (`ambar_volume_quorum`, `ambar_volume_uptodate_replicas`, `ambar_node_allocated_bytes`) и отсутствие горящих алертов `Ambar*`.

### Шаг 4. Обновление документации, `MEMORY.md` и коммит Волны 1
- [x] Зафиксировать фактические результаты Волны 1 в этом документе.
- [x] Обновить `ops/nanopi-r5c-k3s/apps/smarthome/README.md` и `ops/nanopi-r5c-k3s/apps/README.md`.
- [x] Обновить `~/.claude/projects/-Users-mvmalykh-IdeaProjects-ambar/memory/MEMORY.md`.
- [x] Закоммитить изменения Волны 1 в `main` с явным перечнем файлов.

---

## Волна 2 — Оставшиеся сервисы умного дома (отложено до отдельного решения)

- [ ] `smarthome/humidity` → PVC `humidity-conf` (`256Mi`, `ambar-r3-retain`, `fsGroup: 1000`).
- [ ] `smarthome/power` → PVC `power-conf` (`256Mi`, `ambar-r3-retain`, `fsGroup: 1000`).
- [ ] `smarthome/samsung-tv` → PVC `samsung-tv-conf` (`256Mi`, `ambar-r3-retain`, `fsGroup: 1000`).
- [ ] `home/zigbee2mqtt` → PVC `zigbee2mqtt-data` (`1Gi`, `ambar-r3-retain`).

## Волна 3 — Реестр образов и шаблон VPN (отложено до отдельного решения)

- [ ] `registry/registry` + `registry/registry-gc` → PVC `registry-data` (`10Gi`, `ambar-r3-retain`, `uid=0`, `podAffinity` для `registry-gc`, тестовый прогон GC).
- [ ] `vpn/ocserv` (шаблон) → заменить `nfs-client` `100Mi` на `ambar-r3-retain` `256Mi` в `ops/nanopi-r5c-k3s/apps/vpn/ocserv.yaml`.

---

## Фактические результаты Волны 1

**Дата проведения:** 2026-11-21 (14:31–14:36 MSK)

1. **`smarthome/conditioner` → `conditioner-conf` (`256Mi`, `ambar-r3-retain`):**
   - PV/RV: `pvc-8f603357-aad3-40f2-a827-39d7e31b0c41` (`minor: 1000`, `port: 7000`).
   - Контрольные суммы (`VERIFY_OK`, владелец `1000:1000`): `.gitkeep` (`e3b0c442...`), `devices.json` (`6e4206cd...`, 960 B), `devices.json.old` (`62dcd4a7...`, 997 B).
   - Под `conditioner-7cc85df6c7-j629g` запущен на `k3s-3` (`1/1 Running`, 0 рестартов), `/tmp/conditioner.heartbeat` обновляется, обрабатывает MQTT-сообщения.
   - `ReplicatedVolume`: `Primary` на `k3s-3`, `Secondary` на `k3s-1` и `k3s-2`, 3/3 `UpToDate`, `quorum: true`.

2. **`smarthome/presence` → `presence-conf` (`256Mi`, `ambar-r3-retain`):**
   - PV/RV: `pvc-fad76087-30d6-4e30-8695-52a1a1243713` (`minor: 1001`, `port: 7001`).
   - Контрольные суммы (`VERIFY_OK`, владелец `1000:1000`): `.gitkeep` (`e3b0c442...`), `devices.json` (`220e871f...`, 478 B), `devices.json.old` (`b4486526...`, 481 B).
   - Под `presence-5dcb49895c-8b4m6` запущен на `k3s-3` (`1/1 Running`, 0 рестартов), `/tmp/presence.heartbeat` обновляется, обрабатывает `espresense/devices/+/+`.
   - `ReplicatedVolume`: `Primary` на `k3s-3`, `Secondary` на `k3s-1` и `k3s-2`, 3/3 `UpToDate`, `quorum: true`.

3. **`home/mosquitto` → `mosquitto-data` (`512Mi`, `ambar-r3-retain`):**
   - PV/RV: `pvc-6e68294f-91a3-4274-8c29-00a6a740076a` (`minor: 1002`, `port: 7002`).
   - Контрольная сумма (`VERIFY_OK`, владелец `1000:1000`): `mosquitto.db` (`cb50e0c534c1cc347800b78b86978334333920f3be1fd86cc5b6ab9e421ec6e4`, 880329 B; в поде примонтирован с `0660` благодаря `fsGroup: 1000`).
   - Под `mosquitto-79b5c84db9-9kjt2` запущен на `k3s-3` (`1/1 Running`, 0 рестартов), восстановил 2247 base/retained сообщений.
   - Порт `198.18.1.201:1883` отвечает; все 5 сервисов `smarthome` и `zigbee2mqtt` переподключились к брокеру без ошибок.
   - `ReplicatedVolume`: `Primary` на `k3s-3`, `Secondary` на `k3s-1` и `k3s-2`, 3/3 `UpToDate`, `quorum: true`.

4. **Мониторинг VictoriaMetrics и сохранность отката:**
   - `count(ambar_volume_quorum == 1) == 9`, `count(ambar_volume_uptodate_replicas == 3) == 9`.
   - `ambar_node_allocated_bytes` на каждой ноде: `1085972480` байт (~1.01 GiB).
   - Горящих алертов `Ambar*` в `vmalert` нет.
   - Исходные каталоги на NAS (`/volume1/k8s/smarthome/conditioner`, `/volume1/k8s/smarthome/presence`, `/volume1/k8s/mosquitto-data`) оставлены нетронутыми как холодный откат.
