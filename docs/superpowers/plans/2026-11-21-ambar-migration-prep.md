# План реализации подготовительного этапа перед переносом томов с NFS на `ambar`

> **Для агентов-исполнителей:** ОБЯЗАТЕЛЬНЫЙ ПОД-СКИЛЛ: Используйте `superpowers:subagent-driven-development` или `superpowers:executing-plans` для пошаговой реализации плана. Шаги отслеживаются через чекбоксы (`- [ ]`).

**Цель:** подготовить кластер NanoPi R5C (`k3s-1..3`), мониторинг на NAS (`198.18.1.105`) и StorageClass'ы к безопасному переносу боевых томов с NFS на `ambar`, инструментально подтвердив очистку тестовых томов, защиту `Retain`, работу двух подов с одним RWO PVC на одной ноде и переживаемость реального `reboot` узла на LVM-бэкенде.

**Архитектура:** добавляется производственный StorageClass `ambar-r3-retain` (`reclaimPolicy: Retain`, 3 реплики, Protocol C); VictoriaMetrics на NAS подключается к эндпоинтам `:9770/metrics` на `k3s-1..3`; в `vmalert` добавляется группа из 8 правил `ambar` с unit-тестами; в Grafana на NAS провижинится дашборд `ambar-storage`; в изолированном namespace `ambar-verify` проводятся сквозные проверки перед миграцией сервисов.

**Стек технологий:** Kubernetes (`k3s`), `ambar` CSI (`DRBD 9.3.4` + LVM2 `k3s-vg`), VictoriaMetrics, `vmalert`, Alertmanager, Grafana 13.3.0.

## Глобальные ограничения

- Все действия на боевом кластере и NAS выполняются только с разрешения владельца.
- В аннотациях `vmalert` (`summary`, `description`) запрещены символы `<`, `>`, `&` (Telegram `parse_mode: HTML`, проверяется в `ops/alerting/test.sh`).
- Существующие классы `ambar-r3` и `ambar-r2` (`reclaimPolicy: Delete`) не изменяются, чтобы не нарушать бенчмарки `stand/r5c-bench-*.sh`.
- Двуязычная документация (`README.md` / `README.ru.md`, `ops/README.md` / `ops/README.ru.md`) обновляется синхронно.
- В публичных документах `ops/alerting/README.md`, `ops/grafana/README.md`, `ops/nanopi-r5c-k3s/apps/README.md` используются условные адреса `198.51.100.*` и домен `fleet.example`.

---

## Раздел 1. Подготовка плана и локальных конфигураций

### Шаг 1.1. Сохранить план в репозиторий
- [x] Создать файл `docs/superpowers/plans/2026-11-21-ambar-migration-prep.md`.

### Шаг 1.2. Добавить StorageClass `ambar-r3-retain`
- [x] Добавить `StorageClass/ambar-r3-retain` в `deploy/storageclass.yaml` (`reclaimPolicy: Retain`, `replicas: "3"`, `protocol: "C"`, `volumeBindingMode: WaitForFirstConsumer`, `allowVolumeExpansion: false`).

### Шаг 1.3. Добавить скрейп `ambar` в VictoriaMetrics
- [x] Добавить job `ambar` в `ops/nas-monitoring/victoria-metrics/scrape.yaml` для `198.18.1.51..53:9770` с `hostname -> instance`.
- [x] Проверить `docker compose -f ops/nas-monitoring/docker-compose.yaml config -q`.

### Шаг 1.4. Добавить правила алертинга `ambar` в `vmalert` и unit-тесты
- [x] Добавить группу `ambar` (8 правил: `AmbarAgentDown`, `AmbarSplitBrain`, `AmbarVolumeNoQuorum`, `AmbarVolumeSingleReplica`, `AmbarVolumeStandalone`, `AmbarVolumeDegraded`, `AmbarNodeTainted`, `AmbarStorageBudgetHigh`) в `ops/alerting/vmalert/rules.yaml`.
- [x] Создать `ops/alerting/vmalert/tests/ambar.yaml` с позитивными и негативными unit-тестами для всех 8 правил.
- [x] Обновить `ops/alerting/test.sh` (подключить `tests/ambar.yaml` и проверки маршрутизации Alertmanager).
- [x] Запустить `./ops/alerting/test.sh` и убедиться в прохождении всех проверок (`==> all checks passed`).

### Шаг 1.5. Создать Grafana-дашборд `ambar-storage.json`
- [x] Создать `ops/grafana/dashboards/ambar-storage.json` (`uid: "ambar-storage"`, `datasource.uid: "adnsc1wi03doga"`, 4 ряда: Обзор кластера, Тома и репликация, Бюджет LVM и состояние узлов, Производительность DRBD).
- [x] Провалидировать JSON и `datasource.uid` локальным Python-скриптом.

---

## Раздел 2. Применение на кластере и NAS

### Шаг 2.1. Очистить хвост бенчмарков
- [x] Удалить namespace `ambar-bench`.
- [x] Удалить `ReplicatedVolume` `pvc-515295b4-e0a2-4c75-9efc-7cf87c5bccb3` и убедиться, что LV и файлы в `/var/lib/rancher/ambar/{res,volumes}` удалены на всех 3 узлах, а `ambar_node_allocated_bytes == 0`.

### Шаг 2.2. Применить `deploy/storageclass.yaml`
- [x] Выполнить `kubectl apply -f deploy/storageclass.yaml` и проверить наличие `ambar-r3-retain` (`Retain`).

### Шаг 2.3. Доставить `scrape.yaml` на NAS
- [x] Запустить `./ops/nas-monitoring/deploy.sh` (без `--up`).
- [x] Проверить в API VictoriaMetrics (`up{job="ambar"}`), что `k3s-1`, `k3s-2`, `k3s-3` имеют значение `1`.

### Шаг 2.4. Выкатить правила `vmalert`
- [x] Выполнить `KUBECONFIG=~/.kube/k3s-home.yaml ./ops/alerting/deploy.sh`.
- [x] Проверить в `vmalert`, что группа `ambar` (8 правил) загружена и `vmalert_config_last_reload_successful == 1`.

### Шаг 2.5. Выкатить Grafana-дашборд `ambar-storage.json` на NAS
- [x] Выполнить `./ops/grafana/dashboards/deploy.sh --nas ambar-storage.json`.
- [x] Перезапустить `grafana` на NAS (`docker restart grafana`), проверить логи провижининга и `/api/health`.

---

## Раздел 3. Инструментальная проверка на кластере (`ambar-verify`)

### Шаг 3.1. Smoke-тест: `fsGroup: 1000`, совместный RWO на одной ноде и автоудаление `ambar-r3`
- [x] Создать PVC `verify-rwo` (`ambar-r3`, `256Mi`) и под `verify-writer` (`runAsUser: 1000`, `runAsGroup: 1000`, `fsGroup: 1000`), записать `seed.bin` и зафиксировать `sha256sum`.
- [x] Запустить второй под `verify-gc` на той же ноде (`podAffinity`) с тем же PVC, убедиться в чтении `seed.bin` и записи `gc.ok`.
- [x] Удалить оба пода и PVC `verify-rwo`, проверить автоматическое удаление `ReplicatedVolume` и LV на `k3s-1..3`.

### Шаг 3.2. Проверка `ambar-r3-retain` и реального `sudo reboot` узла `k3s-3`
- [x] Создать PVC `verify-retain` (`ambar-r3-retain`, `256Mi`) и Deployment `verify-reboot` (`Recreate`, `tolerationSeconds: 60`, `nodeAffinity` `preferred` 100 на `k3s-3`), записать `phase1.bin` и зафиксировать `sha256sum`.
- [x] Проверить `kubectl get rv`, метрики в VictoriaMetrics и статус `kube-vip`.
- [x] Выполнить `kubectl drain k3s-3 --ignore-daemonsets --delete-emptydir-data`, убедиться в переезде `verify-reboot` и целостности `phase1.bin`.
- [x] Выполнить `ssh mmv@198.18.1.53 'sudo reboot'`; пока `k3s-3` перезагружается, дописать `phase2.bin` и проверить доступность VIP API.
- [x] После возврата `k3s-3` проверить: `findmnt /var/lib/rancher`, автоактивацию LV в `k3s-vg`, загрузку `drbd.ko`, `ambar` `3/3 Running`, возврат DRBD в `UpToDate` (`out_of_sync_bytes == 0`, `standalone_connections == 0`, `manual_check_needed == 0`).
- [x] Выполнить `kubectl uncordon k3s-3` и перепроверить `sha256sum` обоих файлов в `verify-reboot`.

### Шаг 3.3. Проверка политики `Retain` и уборка стенда
- [x] Удалить Deployment `verify-reboot` и PVC `verify-retain`; убедиться, что PV перешёл в `Released`, а `ReplicatedVolume` и LV сохранились на всех 3 нодах.
- [x] Удалить вручную PV, `ReplicatedVolume` и namespace `ambar-verify`; подтвердить отсутствие остатков на `k3s-1..3` и отсутствие горящих алертов `Ambar*`.

---

## Раздел 4. Синхронизация документации и коммит

- [x] Зафиксировать фактические результаты проверок в этом документе.
- [x] Обновить синхронно `README.md` / `README.ru.md`, `ops/README.md` / `ops/README.ru.md`, а также `ops/alerting/README.md`, `ops/grafana/README.md`, `ops/nanopi-r5c-k3s/apps/README.md`.
- [x] Обновить заметки памяти Claude Code (`MEMORY.md`).
- [x] Закоммитить изменения в `main` с явным перечнем файлов.

---

## Фактические результаты проверки

**Дата проведения:** 2026-11-21 (13:58–14:12 MSK)

1. **Очистка хвоста бенчмарков (`ambar-bench` и `rv/pvc-515295b4-e0a2-4c75-9efc-7cf87c5bccb3`):**
   - `ns/ambar-bench` и `rv/pvc-515295b4-...` удалены штатно через финализаторы агентов.
   - На всех 3 узлах `k3s-1..3`: в `k3s-vg` остался только системный `lv_k3s` (35.00 GiB), `VFree: 84.24g`, `/var/lib/rancher/ambar/{res,volumes}` пусты, `ambar_node_allocated_bytes == 0`.
2. **StorageClass `ambar-r3-retain`:** применён на кластере (`reclaimPolicy: Retain`, `volumeBindingMode: WaitForFirstConsumer`, `allowVolumeExpansion: false`, `replicas: 3`, `protocol: C`).
3. **Мониторинг, алертинг и Grafana-дашборд:**
   - `./ops/alerting/test.sh` — все unit-тесты (`uplink`, `nodes`, `heartbeat`, `backups`, `ambar`), проверка отсутствия `< > &` и маршруты `amtool` пройдены (`all checks passed`).
   - VictoriaMetrics на NAS: `up{job="ambar"} == 1` на `k3s-1`, `k3s-2`, `k3s-3`; `ambar_module_info{version="9.3.4"} == 1`.
   - `vmalert`: `vmalert_config_last_reload_successful == 1`, загружены все 8 правил группы `ambar`.
   - Grafana 13.3.0 на NAS: `ambar-storage.json` (`uid: ambar-storage`, 16 панелей, 26 таргетов) провиженен после рестарта (`database: ok`, все 12 дашбордов в таблице `resource` БД Grafana).
4. **Smoke-тест `fsGroup: 1000` и совместного RWO на одной ноде (`ambar-r3`, `pvc-7f2bca70-8c6f-4a8e-a536-7008e77dc705`):**
   - `verify-writer` (`uid=1000`, `gid=1000`, `fsGroup=1000`, `busybox:1.36`) на `k3s-2` получил права на `/data` (`drwxrwsr-x 3 root 1000`) и записал `seed.bin` (`sha256: 9a34829949bb37930bd32a1729bc0a080e4f54907e3e188ab3c042d16ee6ccb8`).
   - `verify-gc` стартовал параллельно на той же ноде (`k3s-2`) с тем же RWO PVC, подтвердил совпадение `sha256sum` и успешно создал `/data/gc.ok` (валидация схемы для пары `registry` + `registry-gc`).
   - Примечание: при первоначальном запуске `verify-writer` с `imagePullPolicy: Never` и полным именем `docker.io/library/busybox:1.36` kubelet выдал `ErrImageNeverPull` — с `busybox:1.36` и `imagePullPolicy: IfNotPresent` запуск прошёл штатно.
   - После удаления подов и PVC `verify-rwo` объект `ReplicatedVolume` и LV удалились на всех 3 узлах автоматически.
5. **Тест `ambar-r3-retain` и реального `sudo reboot` узла `k3s-3` (`pvc-944a99a2-0ce5-4346-8d38-c17a67e5e4f6`):**
   - `verify-reboot` стартовал на `k3s-3`, записал `phase1.bin` (`sha256: a3ac860087730e13a669392d2088d834953d56772af1aea7ba57dd2dc08607ce`).
   - `kubectl drain k3s-3 --ignore-daemonsets --delete-emptydir-data`: под переехал на `k3s-2` за ~37 с, целостность `phase1.bin` подтверждена.
   - `sudo reboot` на `k3s-3` выполнен в `14:08:30 MSK`. Пока узел перезагружался, на `k3s-2` дописан `phase2.bin` (`sha256: dc0ed75c43caa68f7790c6c45dfab30a5b80dc9042318a1aafaf1072b91adffc`), API через VIP `198.18.1.30:6443` работал без перебоев (`kube-vip` на `k3s-2`).
   - В `14:10:15 MSK` `k3s-3` вернулся в статус `Ready`; `findmnt /var/lib/rancher` смонтировал `/dev/mapper/k3s--vg-lv_k3s` (`ext4, rw,noatime`); оба LV (`lv_k3s` и `pvc-944a99a2-...`) автоактивировались.
   - К `14:10:31 MSK` под `ambar-jb6v7` загрузил `drbd 9.3.4 for kernel 6.12.116+deb13-arm64`, применил ресурс `[Up, MarkApplied]` без перезаписи метаданных, открыл LV (`-wi-ao----`) и выполнил ресинк: на всех 3 узлах `disk=UpToDate`, `quorum=true`, `out_of_sync_bytes=0`, `standalone_connections=0`, `manual_check_needed=0`.
   - После `kubectl uncordon k3s-3` `alertmanager-1` и `traefik` вернулись на `k3s-3`; оба контрольных файла (`phase1.bin` и `phase2.bin`) в `verify-reboot` сохранили исходные `sha256sum`.
6. **Проверка `Retain` и уборка:**
   - После удаления `deploy/verify-reboot` и `pvc/verify-retain` PV перешёл в `Released`, а `ReplicatedVolume` и LV на всех 3 нодах сохранились.
   - После ручного удаления PV, `ReplicatedVolume` и `ns/ambar-verify` на всех 3 узлах остался только `lv_k3s` (`VFree: 84.24g`), `ambar_node_allocated_bytes == 0`, горящих алертов `Ambar*` в VictoriaMetrics нет.
