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

## Волна 2 — Оставшиеся сервисы умного дома

- [x] `smarthome/humidity` → PVC `humidity-conf` (`256Mi`, `ambar-r3-retain`, `fsGroup: 1000`).
- [x] `smarthome/power` → PVC `power-conf` (`256Mi`, `ambar-r3-retain`, `fsGroup: 1000`).
- [x] `smarthome/samsung-tv` → PVC `samsung-tv-conf` (`256Mi`, `ambar-r3-retain`, `fsGroup: 1000`).
- [x] `home/zigbee2mqtt` → PVC `zigbee2mqtt-data` (`1Gi`, `ambar-r3-retain`).

## Волна 3 — Реестр образов, шаблон VPN и политика переезда `mosquitto`/`descheduler`

- [x] `registry/registry` + `registry/registry-gc` → PVC `registry-data` (`10Gi`, `ambar-r3-retain`, `uid=0`, `podAffinity` для `registry-gc`, тестовый прогон GC).
- [x] `vpn/ocserv` (шаблон) → заменить `nfs-client` `100Mi` на `ambar-r3-retain` `256Mi` в `ops/nanopi-r5c-k3s/apps/vpn/ocserv.yaml`.
- [x] Защита `mosquitto` и `zigbee2mqtt` от планового выселения (`PodsWithPVC` в `descheduler`) + точечный opt-in `descheduler.alpha.kubernetes.io/evict: "true"` на `smarthome/*` и `registry` + ускорение `readinessProbe` (`1s`/`2s`) и `autosave_interval 300` в `mosquitto`.

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

---

## Фактические результаты Волны 2

**Дата проведения:** 2026-11-21 (14:39–14:44 MSK)

1. **`smarthome/humidity` → `humidity-conf` (`256Mi`, `ambar-r3-retain`):**
   - PV/RV: `pvc-f161b0dd-be5d-445a-ad00-646704780868` (`minor: 1003`, `port: 7003`).
   - Контрольные суммы (`VERIFY_OK`, владелец `1000:1000`): `.gitkeep` (`e3b0c442...`), `devices.json` (`6c68abd8...`, 13328 B), `devices.json.old` (`a0805447...`, 13329 B).
   - Под `humidity-85c76c7d4d-mctvr` запущен на `k3s-3` (`1/1 Running`, 0 рестартов), успешно обновил `/app/conf/devices.json` и отправляет метрики температуры/влажности в VictoriaMetrics, `/tmp/humidity.heartbeat` свежий.
   - `ReplicatedVolume`: `Primary` на `k3s-3`, `Secondary` на `k3s-1` и `k3s-2`, 3/3 `UpToDate`, `quorum: true`.

2. **`smarthome/power` → `power-conf` (`256Mi`, `ambar-r3-retain`):**
   - PV/RV: `pvc-8bee4b17-7274-4ec7-9670-659dc81fa022` (`minor: 1004`, `port: 7004`).
   - Контрольные суммы (`VERIFY_OK`, владелец `1000:1000`): `.gitkeep` (`e3b0c442...`), `devices.json` (`742ca6b0...`, 1428 B), `devices.json.old` (`bdbdc5dc...`, 1428 B).
   - Под `power-69c8d88f6b-dm6qm` запущен на `k3s-3` (`1/1 Running`, 0 рестартов), обновил `/app/conf/devices.json` и публикует `python2mqtt/power/total`, `/tmp/power.heartbeat` свежий.
   - `ReplicatedVolume`: `Primary` на `k3s-3`, `Secondary` на `k3s-1` и `k3s-2`, 3/3 `UpToDate`, `quorum: true`.

3. **`smarthome/samsung-tv` → `samsung-tv-conf` (`256Mi`, `ambar-r3-retain`):**
   - PV/RV: `pvc-b457ee9a-c3ec-49d7-9217-40299b21035e` (`minor: 1005`, `port: 7005`).
   - Источник NAS: `/volume1/k8s/smarthome/samsung_tv`. Контрольные суммы (`VERIFY_OK`, владелец `1000:1000`): `.gitkeep` (`e3b0c442...`), `devices.json` (`b0a93ad7...`, 468 B), `devices.json.old` (`b7127c43...`, 469 B).
   - Под `samsung-tv-5bf8b7d9d9-cwbds` запущен на `k3s-3` (`hostNetwork: true`, `1/1 Running`, 0 рестартов), подписан на `python2mqtt/tv/#`, `/tmp/samsung_tv.heartbeat` свежий.
   - `ReplicatedVolume`: `Primary` на `k3s-3`, `Secondary` на `k3s-1` и `k3s-2`, 3/3 `UpToDate`, `quorum: true`.

4. **`home/zigbee2mqtt` → `zigbee2mqtt-data` (`1Gi`, `ambar-r3-retain`):**
   - PV/RV: `pvc-cf3e6165-f6df-471f-8527-c0ac0ce6b5ea` (`minor: 1006`, `port: 7006`).
   - Контрольная сверка (`VERIFY_OK`, 11 файлов включая `device_icons/`, `configuration.yaml`, `database.db`, `coordinator_backup.json`, `state.json`, владелец `1000:1000`).
   - Под `zigbee2mqtt-5d745df558-9r492` запущен на `k3s-2` (`1/1 Running`, 0 рестартов), ASH/EZSP стартовал (`Network up`, `Adapter network matches config`), frontend `:8080` отвечает `200`, телеметрия устройств поступает в `mosquitto` и `power`.
   - `ReplicatedVolume`: `Primary` на `k3s-2`, `Secondary` на `k3s-1` и `k3s-3`, 3/3 `UpToDate`, `quorum: true`.

5. **Мониторинг VictoriaMetrics и сохранность отката (итого после Волн 1 и 2):**
   - `count(ambar_volume_quorum == 1) == 21`, `count(ambar_volume_uptodate_replicas == 3) == 21` (7 боевых томов × 3 узла).
   - `ambar_node_allocated_bytes` на каждой ноде: `2981232640` байт (~2.78 GiB).
   - Горящих алертов `Ambar*` в `vmalert` нет.
   - Исходные каталоги на NAS (`/volume1/k8s/smarthome/{humidity,power,samsung_tv}`, `/volume1/k8s/zigbee2mqtt-data`) оставлены нетронутыми как холодный откат.

---

## Фактические результаты Волны 3 и настройки `mosquitto`/`descheduler`

**Дата проведения:** 2026-11-21 (15:04–15:09 MSK)

1. **`registry/registry` + `registry-gc` → `registry-data` (`10Gi`, `ambar-r3-retain`):**
   - PV/RV: `pvc-f267e05c-0b2a-493f-9d5e-ffb36d81eec9` (`minor: 1007`, `port: 7007`).
   - Перенос через `pod/registry-migrator` (`uid=0`, `busybox:1.36`): `src_files=533 dst_files=533`, `VERIFY_OK` без расхождений по `sha256sum`.
   - Под `registry-6557d7dbc4-9zmjr` запущен на `k3s-3` (`1/1 Running`, 0 рестартов, `/dev/drbd1007` смонтирован на `/var/lib/registry`), внешний `GET https://registry.k3s.beerloga.su/v2/` → `401`, `/v2/_catalog` → `["conditioner","humidity","outline-ui","power","presence","probe","samsung_tv"]`.
   - `ReplicatedVolume`: `Primary` на `k3s-3`, `Secondary` на `k3s-1` и `k3s-2`, 3/3 `UpToDate`, `quorum: true`.
   - Тестовый ручной Job `registry-gc-verify` (`registry-gc-verify-8xkth`) запланировался через `podAffinity` на `k3s-3` и успешно завершился за 5 с (`158 blobs marked, 0 blobs and 0 manifests eligible for deletion`) без прерывания работы `registry`, после чего удалён.
2. **Шаблон `ops/nanopi-r5c-k3s/apps/vpn/ocserv.yaml`:**
   - PVC `ocserv-passwd` обновлён на `storageClassName: ambar-r3-retain`, `storage: 256Mi` (в кластер не применялся из-за плейсхолдера `<REGISTRY>/<TAG>`).
3. **Защита `mosquitto`/`zigbee2mqtt` и настройка `descheduler`:**
   - В `ops/nanopi-r5c-k3s/apps/descheduler/descheduler.values.yaml` оставлена защита `PodsWithPVC` (защищает `mosquitto`, `zigbee2mqtt`, `nfs-client` и будущие PVC по умолчанию; комментарий актуализирован).
   - В шаблоны подов `smarthome/{conditioner,humidity,power,presence,samsung-tv}.yaml` и `registry/registry.yaml` добавлена opt-in аннотация `descheduler.alpha.kubernetes.io/evict: "true"` — они разрешены для ночной балансировки, а `mosquitto` и `zigbee2mqtt` остаются неподвижными при штатной работе.
   - В `ops/nanopi-r5c-k3s/apps/home/mosquitto.yaml` задан `autosave_interval 300` (5 мин) и ускорена `readinessProbe` (`initialDelaySeconds: 1`, `periodSeconds: 2`). Восстановлено 2247 retained-сообщений, VIP `198.18.1.201:1883` отвечает, все 5 сервисов `smarthome` (`1/1 Running`, 0 рестартов, свежие heartbeat) и `zigbee2mqtt` обмениваются сообщениями.
4. **Распределение подов и мониторинг VictoriaMetrics (финал всех 3 волн):**
   - Распределение по узлам: `k3s-1` (`11` подов / `7` без DS / `192Mi` memory req), `k3s-2` (`11` / `7` / `358Mi`), `k3s-3` (`14` / `10` / `544Mi` — в `04:15 MSK` `descheduler` сможет разгрузить до 2 сервисов `smarthome`/`registry` с `k3s-3` на `k3s-1`).
   - Все 8 боевых PVC (`mosquitto-data`, `zigbee2mqtt-data`, 5× `smarthome/*-conf`, `registry-data`) в `Bound` (`ambar-r3-retain`).
   - VictoriaMetrics: `count(ambar_volume_quorum == 1) == 24`, `count(ambar_volume_uptodate_replicas == 3) == 24` (8 томов × 3 узла), `ambar_node_allocated_bytes == 13720510464` байт (~12.78 GiB) на каждой ноде, `vmalert_alerts_firing{alertname=~"Ambar.*"} == 0`.
   - Исходные каталоги `/volume1/k8s/*` на NAS и `nfs-client` в `kube-system` сохранены нетронутыми.
