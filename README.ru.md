# ambar

[English version](README.md)

`ambar` — CSI-драйвер Kubernetes для реплицируемых томов на небольших кластерах
k3s. Репликацию выполняет модуль ядра DRBD 9; драйвер — один бинарь на Rust,
запущенный как DaemonSet.

## Состояние

| Часть | Состояние |
|---|---|
| Спека | приведена к коду фазы 3; открытые вопросы — в её конце |
| Образ с собранным модулем DRBD 9.3.4 и drbd-utils 9.35.0 | собирается, есть тесты |
| Стенд: четыре ВМ Lima с k3s | скрипты в `stand/` |
| Опыты с отказами DRBD 9 на стенде | записаны с выводом команд |
| CSI-драйвер | создаёт, монтирует и удаляет тома через PVC на стенде (`stand/csi-e2e.sh`); обрабатывает отказы узлов и сети (`stand/failure-e2e.sh`) |
| Смена набора копий существующего тома (фаза 2б) | реализована: команды CLI `ambar replica add`, `remove`, `move` с правилами безопасности и перебалансировкой свидетеля (`stand/replica-e2e.sh`) |
| Обработка отказов узлов (фаза 3): освобождение тома при потере кворума, taint `no-quorum`, `adjust` при `StandAlone`, метрики | реализована: taint `no-quorum`, grace period 30 с и выселение подов, авто-adjust `StandAlone`, очистка Client, метрики Prometheus на `:9770/metrics` (`stand/failure-e2e.sh`) |
| Запуск на боевом кластере | ambar развернут на боевом кластере из 3 узлов NanoPi R5C в namespace `ambar-system`, работает параллельно с существующими боевыми сервисами |
| Замеры на целевом железе (NanoPi R5C) | выполнены, ссылка на протокол [`docs/superpowers/specs/2026-11-19-ambar-phase4-r5c-results.md`](docs/superpowers/specs/2026-11-19-ambar-phase4-r5c-results.md) |
| Прямой блочный LVM бэкенд (фаза 5) | реализован и проверен на кластере NanoPi R5C; задержка 4K `fsync` снижена до 12.65 мс (r3) / 10.03 мс (r2) без loopback и двойного журналирования ext4 ([спека](docs/superpowers/specs/2026-11-19-ambar-phase5-lvm-backend.md)) |
| Отказоустойчивый Control Plane (kube-vip) | реализован и работает на боевом 3-нодовом кластере NanoPi R5C: виртуальный IP `198.51.100.30:6443` с автоматическим ARP failover и скриптами в [`stand/r5c-kube-vip.sh`](stand/r5c-kube-vip.sh) |
| Эксплуатация кластера и нагрузки | манифесты нагрузок, гайды по подготовке железа, конфиги мониторинга и алертинга вынесены в проект [`infra-repo`](file:///home/user/IdeaProjects/infra-repo/) |

## Что делает драйвер

- У тома `replicas: N` копий на N узлах (N не меньше 2). При чётном N ещё один
  узел подключается к тому без диска как свидетель. Любой другой узел
  подключается к тому без диска, когда на него назначен под с этим томом, и
  отключается после ухода пода.
- Существующие тома могут менять количество копий и их размещение на лету (`ambar replica add`,
  `remove`, `move`) без прерывания ввода-вывода.
- Автоматическая обработка потери кворума: при потере кворума DRBD на смонтированном томе на узел выставляется taint `ambar.balookrd.github.io/no-quorum:NoSchedule`. По истечении grace period (30 с) агент выселяет под через Kubernetes API, позволяя перепланировать нагрузку на узел с кворумом. Таинт автоматически снимается при возврате кворума.
- Автоматическое восстановление: соединения в состоянии `StandAlone` на узлах в роли Secondary автоматически переподключаются (`drbdadm adjust`), синхронизируя диск в `UpToDate`. При исчерпании 3 попыток (split-brain) выставляется метрика `ambar_volume_manual_check_needed`.
- Автоматическая очистка клиентов: когда поды на узле `Client` завершаются, агент останавливает ресурс DRBD (`drbdadm down`) и очищает запись из `spec.nodes`, освобождая слот пира.
- Встроенный сервер метрик Prometheus на HTTP порту 9770 (`:9770/metrics`).
- При `protocol: C` запись подтверждается после того, как она легла на диски
  всех узлов с копией.
- Том монтирует один узел (`ReadWriteOnce`, `ReadWriteOncePod`).
- Данные тома поддерживают два типа бэкенда:
  - **Логические тома LVM** (`--volume-group=k3s-vg`, рекомендуется для bare-metal): модуль DRBD работает напрямую с блочными устройствами (`/dev/k3s-vg/pvc-<UUID>`), исключая накладные расходы loopback-устройств и двойного журналирования ext4/JBD2.
  - **Разреженные файлы** (`--volumes-dir=/var/lib/ambar/volumes`, тестовый стенд/обратная совместимость): подключаются как loop-устройства с прямым вводом-выводом (direct I/O) без необходимости переразметки диска.
- Модуль DRBD собирается заранее под точную версию ядра и лежит в образе;
  компилятор на узлах не нужен.

## Смена набора копий

Копии тома можно добавлять, удалять и переносить между узлами на лету без остановки рабочей нагрузки пода:

```bash
# Добавить копию на node-2 (ждёт завершения синхронизации до UpToDate)
kubectl -n ambar-system exec ds/ambar -c ambar -- ambar replica add my-vol node-2

# Снять копию с node-3 (ждёт безопасного удаления копии агентом)
kubectl -n ambar-system exec ds/ambar -c ambar -- ambar replica remove my-vol node-3

# Перенести копию с node-2 на node-4 (добавляет на node-4, ждёт UpToDate, затем снимает с node-2)
kubectl -n ambar-system exec ds/ambar -c ambar -- ambar replica move my-vol node-2 node-4
```

Флаги:
- `--namespace <ns>`: пространство имён объекта тома (по умолчанию: `ambar-system`).
- `--timeout <секунды>`: предельное время ожидания синхронизации или снятия в секундах (по умолчанию: `0` — без ограничения).

Коды выхода:
- `0`: изменение набора копий успешно завершено.
- `1`: ошибка, истечение таймаута или отказ по правилам безопасности (например, попытка снять копию с активного узла Primary либо оставить меньше 2 копий).

## Обработка отказов и метрики

Агент непрерывно отслеживает состояние ресурсов DRBD, сетевой кворум и соединения на каждом узле:

- **Потеря кворума и выселение подов**: При потере кворума DRBD на смонтированном томе на узел немедленно выставляется taint `ambar.balookrd.github.io/no-quorum:NoSchedule`. Если кворум не восстанавливается в течение grace period (30 с), агент удаляет локальные поды с этим томом, позволяя Kubernetes перепланировать их на здоровый узел с кворумом.
- **Авто-восстановление и split-brain**: При переходе соединения DRBD в `StandAlone` на узле в роли Secondary агент выполняет `drbdadm adjust` каждые 5 с (до 3 попыток). При успехе (`Connected`) диск синхронизируется в `UpToDate`. Если соединение остаётся `StandAlone` после 3 попыток, метрика `ambar_volume_manual_check_needed` переходит в `1`, сигнализируя о необходимости ручного вмешательства (и сбрасывается в `0` после восстановления связи).
- **Очистка забытых клиентов**: При завершении или удалении пода на узле `Client` агент автоматически опускает ресурс DRBD (`drbdadm down`) и удаляет запись узла из `spec.nodes`, освобождая слот пира.
- **Метрики Prometheus**: Агент отдаёт метрики по HTTP на порту 9770 (`:9770/metrics`).

Флаги CLI агента:
- `--failure-poll-interval <секунды>`: интервал проверки отказов (по умолчанию: `2s`).
- `--quorum-grace-period <секунды>`: время ожидания перед выселением подов при потере кворума (по умолчанию: `30s`).
- `--adjust-attempts <число>`: максимальное число попыток авто-adjust для `StandAlone` (по умолчанию: `3`).
- `--adjust-interval <секунды>`: задержка между попытками adjust (по умолчанию: `5s`).
- `--metrics-port <порт>`: HTTP-порт сервера метрик Prometheus (по умолчанию: `9770`).
- `--metrics-bind <ip>`: адрес привязки сервера метрик (по умолчанию: `0.0.0.0`).

Экспортируемые метрики Prometheus:
- `ambar_node_allocated_bytes{node}`: суммарный выделенный объём томов на узле в байтах.
- `ambar_node_tainted{node}`: `1`, если на узле выставлен taint `no-quorum:NoSchedule`, иначе `0`.
- `ambar_volume_uptodate_replicas{node, volume}`: число дисковых реплик в состоянии `UpToDate` для данного тома.
- `ambar_volume_out_of_sync_bytes{node, peer, volume}`: объём несинхронизированных данных с пиром в байтах.
- `ambar_volume_quorum{node, volume}`: `1`, если у тома на узле есть кворум DRBD, иначе `0`.
- `ambar_volume_standalone_connections{node, volume}`: число соединений тома в состоянии `StandAlone`.
- `ambar_volume_manual_check_needed{node, volume}`: `1`, если исчерпаны попытки adjust (алерт split-brain), иначе `0`.
- `ambar_module_info{version}`: информационная метрика с версией модуля ядра DRBD.

Классы хранения в [`deploy/storageclass.yaml`](deploy/storageclass.yaml): `ambar-r3` (3 копии, `Delete`), `ambar-r2` (2 копии + witness, `Delete`) и `ambar-r3-retain` (3 копии, `Retain`, рекомендуемый класс для боевых нагрузок). Конфигурация скрейпа VictoriaMetrics, правила `vmalert` и дашборд Grafana `ambar-storage` находятся в выделенном проекте [`infra-repo`](file:///home/user/IdeaProjects/infra-repo/) в каталоге `ops/`.

## Замеры на целевом железе (NanoPi R5C)

Испытания фаз 4 и 5 проводились на действующем трехнодовом кластере FriendlyElec NanoPi R5C (Rockchip RK3568B2, 4 ядра Cortex-A55, 4 ГБ ОЗУ, NVMe 128 ГБ, сеть репликации 2.5 GbE, Debian 13 trixie, ядро Linux 6.12.116+, k3s v1.36.2+k3s1). Протоколы: фаза 4 [`docs/superpowers/specs/2026-11-19-ambar-phase4-r5c-results.md`](docs/superpowers/specs/2026-11-19-ambar-phase4-r5c-results.md) и фаза 5 [`docs/superpowers/specs/2026-11-19-ambar-phase5-lvm-backend.md`](docs/superpowers/specs/2026-11-19-ambar-phase5-lvm-backend.md).

| Метрика / Параметр теста | `local-path` (baseline) | `ambar-r3` (3 дисковые реплики) | `ambar-r2` (2 реплики + witness) | Longhorn 1.12.2 (2026-11-09) | Целевой порог | Результат |
|---|---|---|---|---|---|---|
| **cgroup v2 anon memory подов (max на узел)** | 0 (в ядре) | **39–45 МиБ** | — | — | $\le$ 64 МиБ | **PASS** |
| **Дельта RSS `k3s-server` к базовой** | 0.00 МиБ | **+4.4 .. +29.1 МиБ** | — | 180–450 МиБ | $\le$ 50 МиБ | **PASS** |
| **Задержка `fsync` 4K: чистый LVM (фаза 5, средняя)** | — | **13.25 мс** | **10.33 мс** | 13.6 мс | $\le$ 5.0 мс | **PASS (в 2.4–2.8 раза быстрее)** |
| Задержка `fsync` 4K: чистый LVM (p50 / p95 / p99) | — | **12.65** / 18.22 / 27.66 мс | **10.03** / 14.09 / 16.91 мс | 12.1 / 18.4 / 24.8 мс | — | Справочно |
| Задержка `fsync` 4K: loopback (фаза 4, средняя) | **2.58 мс** | 30.86 мс | 28.35 мс | 13.6 мс | $\le$ 5.0 мс | Прежний loopback |
| Задержка `fsync` 4K: loopback (p50 / p95 / p99) | 2.54 / 3.65 / 6.00 мс | 30.28 / 38.54 / 44.83 мс | 27.92 / 36.44 / 40.11 мс | 12.1 / 18.4 / 24.8 мс | — | Справочно |
| Кворум томов под 10-мин нагрузкой | N/A | **5/5 (100%)** | — | Нестабилен | 100% | **PASS** |
| Разорванные соединения (split-brain) | N/A | **0** | — | Были | 0 | **PASS** |

Ключевые выводы:
- **Потребление памяти**: Поды драйвера потребляют всего 39–45 МиБ anon-памяти на узел (значительно ниже лимита 64 МиБ и несравнимо лучше Longhorn с 145–348 МиБ). Прирост памяти управляющего процесса `k3s-server` составил всего +4.4..+29.1 МиБ (против 180–450 МиБ у Longhorn).
- **Надежность репликации**: 100% удержание кворума и 0 split-brain в течение 10-минутного непрерывного стресс-теста на 5 томах.
- **Задержка ввода-вывода**: На прямом LVM бэкенде (фаза 5) медианная задержка 4K `fsync` (p50) снизилась до **12.65 мс** для `ambar-r3` (3 копии) и до **10.03 мс** для `ambar-r2` (2 копии + witness), показав **ускорение в 2.4–2.8 раза** по сравнению с loopback-бэкендом фазы 4 (30.28 мс и 27.92 мс) и рост IOPS транзакционной записи на +127%..+167% (с 32 до 73 IOPS на `ambar-r3` и с 35 до 93 IOPS на `ambar-r2`). Это подтверждает устранение накладных расходов loopback-слоя и двойного журналирования ext4/JBD2. Оставшаяся задержка определяется физическим временем сброса кэша NVMe-накопителей (2.58 мс на local-path), реплицируемым по сети в синхронном режиме Protocol C.

## Структура репозитория

| Путь | Содержимое |
|---|---|
| `docs/superpowers/specs/2026-11-12-ambar-design.md` | спека |
| `docs/superpowers/specs/2026-11-12-drbd9-spike-results.md` | протокол опытов с DRBD 9 и проверок на стенде |
| `docs/superpowers/specs/2026-11-19-ambar-phase4-r5c-results.md` | протокол испытаний и замеры производительности на NanoPi R5C |
| `docs/superpowers/specs/2026-11-19-ambar-phase5-lvm-backend.md` | дизайн LVM бэкенда и протокол замеров производительности |
| `docs/superpowers/plans/` | исполненные планы фаз 0, 1, 2а и 2б (исторические) |
| `src/` | бинарь `ambar` (Rust): агент узла, цикл согласования, сервисы CSI |
| `proto/csi.proto`, `build.rs` | описание протокола CSI v1 и генерация кода по нему |
| `deploy/` | манифесты Kubernetes: CRD, CSIDriver, RBAC, DaemonSet, StorageClass, образец `ConfigMap ambar-nodes` |
| `image/` | Dockerfile, загрузчик модуля, тесты образа |
| `stand/` | скрипты стенда, установка драйвера и сквозная проверка; см. `stand/README.md` |
| `stand/spike/` | скрипты опытов и образцы `drbdsetup status --json` |
| `.github/` | GitHub Actions CI, workflow релизов CalVer и вспомогательные скрипты |
| `cliff.toml`, `CHANGELOG.md`, `CHANGELOG.ru.md` | конфигурация `git-cliff` для release notes и двуязычная история изменений |
| `scripts/code-index.py`, `sgconfig.yml`, `.mcp.json` | локальный AST-индекс кода (`.code-index/`, см. [`docs/CODE-INDEX.ru.md`](docs/CODE-INDEX.ru.md)) и конфигурация `ast-grep` |

[`scripts/code-index.py`](scripts/code-index.py) генерирует локальную
AST-карту репозитория в `.code-index/` (через `ast-grep`, с fallback на regex), а
[`sgconfig.yml`](sgconfig.yml) и [`.mcp.json`](.mcp.json) подключают AST-поиск и
рефакторинг через `ast-grep`; подробности — в
[`docs/CODE-INDEX.ru.md`](docs/CODE-INDEX.ru.md).

## Архитектура хранилища узла (LVM)

На bare-metal кластерах (включая целевой кластер FriendlyElec NanoPi R5C) накопитель NVMe (`/dev/nvme0n1`, ~120 ГБ) организован по LVM-топологии:

```text
+--------------------------------------------------------------------------+
|                       Physical Volume /dev/nvme0n1                       |
|                         (Группа томов: k3s-vg)                           |
+------------------------------------+-------------------------------------+
| Системный LV: lv_k3s (35 ГБ, ext4) | Свободные PE: ~84 ГБ                |
| Монтирование: /var/lib/rancher     | Динамические LV под тома ambar      |
| (состояние k3s, образы, логи)      | (/dev/k3s-vg/pvc-<UUID>) -> DRBD 9  |
+------------------------------------+-------------------------------------+
```

- **Physical Volume и Volume Group**: Накопитель NVMe целиком отдан под Physical Volume в составе Volume Group `k3s-vg` (размер PE = 4 МиБ).
- **Системный LV (`lv_k3s`)**: Линейный том объемом 35 ГБ с файловой системой `ext4` (`LABEL=k3s-data`), смонтированный в точку `/var/lib/rancher` с опциями `noatime,nofail`. На нем располагаются runtime-данные k3s, база etcd и образы контейнеров.
- **Динамические блочные тома ambar**: Оставшиеся ~84 ГБ свободных PE динамически нарезаются агентом `ambar-agent` под блочные реплицируемые тома DRBD.
- **Прямая передача блочных устройств в DRBD**: DRBD 9 работает с блочным устройством `/dev/k3s-vg/pvc-<UUID>` напрямую. Это исключает промежуточный слой VFS хостовой файловой системы, убирает loopback-устройства и ликвидирует двойное журналирование (`ext4` внутри тома + хостовый `ext4/JBD2`).

## Подготовка узлов кластера (NanoPi R5C)

Для развертывания на «чистых» узлах Debian 13 предусмотрены скрипты автоматизации в каталоге `stand/`:

1. **Инициализация LVM-топологии хранилища**:
   Разметка NVMe в VG `k3s-vg` и LV `lv_k3s` (35 ГБ под `/var/lib/rancher`):
   ```bash
   # На всех узлах последовательно:
   stand/r5c-init-node.sh all
   # Либо на конкретном узле:
   stand/r5c-init-node.sh k3s-1
   ```

2. **Развертывание кластера k3s**:
   Инициализация кластера на первом узле и подключение остальных нод через интерконнект репликации `lan0`:
   ```bash
   # Инициализация первого узла (cluster-init):
   stand/r5c-install-k3s.sh k3s-1

   # Подключение остальных узлов (токен считывается автоматически с первого узла):
   stand/r5c-install-k3s.sh k3s-2
   stand/r5c-install-k3s.sh k3s-3
   ```

3. **Развертывание ambar с LVM бэкендом**:
   Запуск DaemonSet с аргументом `--volume-group=k3s-vg` и квотами емкости узлов в `ConfigMap ambar-nodes`:
   ```bash
   stand/r5c-deploy.sh
   ```

## Требования для образа и стенда

| Что | Значение |
|---|---|
| Хост | macOS на Apple silicon |
| Lima | 2.3.0, `vmType: vz` |
| Docker | `docker buildx`, `linux/arm64` |
| Ядро, под которое собран модуль | `6.12.116+deb13-arm64` (штатное ядро Debian 13) |
| Rust | stable 1.100 или новее (`rust-version` в `Cargo.toml`) |
| Для `image/build.sh` на хосте | `cargo-zigbuild`, `zig` и цель, добавленная командой `rustup target add aarch64-unknown-linux-musl` (проверено с zig 0.17.1 и rustc 1.100.0). Сборка использует `jemalloc` по умолчанию с 4-КиБ страницей для `aarch64` Linux, закреплённой в `.cargo/config.toml`. |

## Сборка и проверка образа

Команды выполняются из корня репозитория.

```bash
image/build.sh
```

```bash
image/test.sh
```

Ожидаемая последняя строка:
`OK: ambar-drbd:dev module 9.3.4 for 6.12.116+deb13-arm64, drbd-utils 9.35.0`

```bash
image/test-loader.sh
```

Ожидаемая последняя строка: `OK: 6 loader cases`. Этому тесту не нужны ни
Docker, ни ядро Linux.

## Проверка кода

```bash
cargo test
```

Ожидаемая строка: `test result: ok. 921 passed`. У кода монтирования есть
тесты, которые выполняются только под Linux; на macOS запустите тесты ещё и в
контейнере:
`docker run --rm -v "$PWD":/src -w /src rust:1-slim cargo test --locked`.

## Установка на стенд

Запуск стенда, установка драйвера из `deploy/` скриптом `stand/install.sh` и
сквозная проверка `stand/csi-e2e.sh` описаны в `stand/README.md`. Вне стенда
манифесты не применялись; для другого кластера образ и `ConfigMap ambar-nodes`
задаются под этот кластер (см. комментарии в `deploy/daemonset.yaml` и
`deploy/nodes-configmap.example.yaml`).

## Эксплуатация кластера и рабочие нагрузки

Все материалы по эксплуатации боевого кластера из трёх плат NanoPi R5C (`k3s-1..3`), подготовке железа, ingress, мониторингу и нагрузкам вынесены в выделенный проект [`infra-repo`](file:///home/user/IdeaProjects/infra-repo/):

- [`infra-repo/ops/`](file:///home/user/IdeaProjects/infra-repo/ops/README.ru.md): Манифесты боевого кластера, ingress Traefik, L2-балансировщик MetalLB и перенесённые сервисы (`smarthome`, `ingress`, `registry` и др.).
- [`infra-repo/ops/nanopi-r5c-k3s/`](file:///home/user/IdeaProjects/infra-repo/ops/nanopi-r5c-k3s/README.md): Подробное руководство по настройке узлов, сетевая топология (`wan0` 198.51.100.0/24, `lan0` 10.10.10.0/24), разметка NVMe под LVM и отказоустойчивый Control Plane на базе kube-vip.
- [`stand/`](stand/README.md#физический-кластер-nanopi-r5c-скрипты-r5c-sh): Скрипты автоматизации обслуживания физического кластера (`stand/r5c-*.sh`), включая онлайн-миграцию на LVM (`stand/r5c-migrate-lvm.sh`), управление kube-vip (`stand/r5c-kube-vip.sh`) и замеры производительности fio.
- [`infra-repo/ops/alerting/`](file:///home/user/IdeaProjects/infra-repo/ops/alerting/README.md) и [`infra-repo/ops/grafana/`](file:///home/user/IdeaProjects/infra-repo/ops/grafana/README.md): Правила алертов VictoriaMetrics и дашборды Grafana для мониторинга кластера и хранилища.

## Релизы

`ambar` использует теги CalVer (`vYY.M.PATCH` без ведущего нуля у месяца,
например `v26.11.0`), публикуемые как GitHub Releases (*Tag Release*). В каждый
релиз входят статические Linux `musl`-бинарники
(`ambar-<version>-aarch64-unknown-linux-musl.tar.gz` и
`ambar-<version>-x86_64-unknown-linux-musl.tar.gz`), файл `SHA256SUMS.txt` и
автоматически сгенерированные release notes. Контейнерный образ `ambar-drbd`,
содержащий модуль ядра DRBD под конкретный `KVER` хоста, по-прежнему
собирается локально через `image/build.sh`. Версия схемы Kubernetes CRD
(`v1alpha1`) остаётся независимой от тегов релизов.

Ручной workflow [*Release*](.github/workflows/release.yml) принимает
необязательную серию `YY.M` (по умолчанию текущий месяц UTC), вычисляет
следующий patch-номер по существующим тегам `vYY.M.*`, обновляет `Cargo.toml`
и `Cargo.lock` с помощью [`.github/scripts/version.py`](.github/scripts/version.py),
отправляет в `main` коммит `release: cut v<версия>` и печатает команду
`git tag -s v<версия>` для локального запуска. Push подписанного тега запускает
[*Tag Release*](.github/workflows/tag-release.yml), который проверяет синхронность
версий в манифестах, собирает оба `musl`-бинарника, формирует release notes
через `git-cliff` ([`.github/scripts/gen-release-notes.sh`](.github/scripts/gen-release-notes.sh))
и публикует GitHub Release со статусом `latest`. Подробнее — в [CHANGELOG.ru.md](CHANGELOG.ru.md).

## Лицензия

Apache License 2.0, текст в файле [LICENSE](LICENSE).
