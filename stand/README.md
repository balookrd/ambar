# Стенд ambar

Три ВМ Lima (`dv-1`, `dv-2`, `dv-3`) с Debian 13 trixie arm64, ядром
`6.12.116+deb13-arm64`, k3s из трёх server и привилегированным DaemonSet
`toolbox` с образом `ambar-drbd` (модуль DRBD 9.3.4, `drbd-utils` 9.35.0).

Префикс `dv` в именах ВМ — историческое сокращение первого названия проекта `duovol` (затем `sklad`, сейчас `ambar`); ВМ не переименованы, потому что для этого Lima-инстанс пришлось бы создавать заново.

Стенд нужен для проверок из `docs/superpowers/specs/2026-11-12-drbd9-spike-results.md`.

## Требования

| Что | Значение |
|---|---|
| Хост | macOS на Apple silicon |
| Lima | 2.3.0, `vmType: vz`, сеть `user-v2` (`stand/lima/node.yaml`) |
| Docker | `docker buildx`, сборка под `linux/arm64` |
| Для `image/build.sh` на хосте | Rust stable 1.95 или новее, `cargo-zigbuild`, `zig`, цель `rustup target add aarch64-unknown-linux-musl` (проверено с zig 0.17.1 и rustc 1.99.1) |
| Ресурсы | по 2 CPU, 3 ГиБ RAM и 20 ГиБ диска на ВМ |

## Порядок запуска

Команды выполняются из корня репозитория.

1. Соберите образ: `image/build.sh`. Переменные: `KVER` (по умолчанию
   `6.12.116+deb13-arm64`), `IMAGE` (по умолчанию `ambar-drbd:dev`).
2. Создайте ВМ: `stand/up.sh`. Скрипт запускает ВМ, при несовпадении ядра ставит
   `linux-image-$KVER`, ставит `nftables` и вызывает `stand/check.sh`. Ожидаемый
   конец вывода: `OK: stand is up`.
3. Поставьте k3s и toolbox: `stand/k3s.sh`. Скрипт кладёт чёрный список `drbd` в
   `/etc/modprobe.d`, ставит k3s версии `K3S_VERSION` (по умолчанию
   `v1.37.2+k3s1`), импортирует образ `IMAGE` в containerd каждого узла,
   перезапускает DaemonSet `toolbox` и загружает модуль. Повторный запуск не
   переустанавливает k3s и подхватывает пересобранный образ с тем же тегом.

Ветка установки ядра в `stand/up.sh` (`apt-get install linux-image-$KVER`,
`update-grub`, перезапуск ВМ) не выполнялась ни разу.

## Переменные

| Переменная | По умолчанию | Что задаёт |
|---|---|---|
| `NET_IF` | `eth0` | Интерфейс общей сети внутри ВМ; с него `vm_ip` берёт адрес узла |
| `KVER` | `6.12.116+deb13-arm64` | Версия ядра, которую проверяют `up.sh` и `check.sh` |
| `IMAGE` | `ambar-drbd:dev` | Образ, который `k3s.sh` импортирует в узлы |
| `K3S_VERSION` | `v1.37.2+k3s1` | Версия k3s для `INSTALL_K3S_VERSION` |

## Работа со стендом

`stand/lib.sh` содержит функции, которые подключаются через `source`:

| Функция | Что делает |
|---|---|
| `vm <узел> <команда>` | Выполняет команду от root внутри ВМ |
| `tb <узел> <команда>` | Выполняет команду в поде `toolbox` этого узла |
| `kc <аргументы kubectl>` | Запускает `kubectl` на первом узле из `NODES`, который в Lima в состоянии `Running` и чей API отвечает |

После перезапуска ВМ модуль DRBD не загружен. Загрузите его:

```bash
bash -c 'source stand/lib.sh; tb dv-2 "/opt/ambar/load-module.sh"'
```

Ресурсы DRBD, loop-устройства и вспомогательные скрипты проверок удаляет
`stand/spike/clean.sh`. ВМ и их диски удаляет `stand/down.sh`.

## Опыты перед CSI

Скрипт `stand/spike/pre-csi.sh <p7|p8|p9|p10|clean>` выполняет опыты P7 (`drbdadm dump-md`), P8 (отключение стороны без диска от работающего ресурса с писателем), P9 (`blkid -p` и чтение по смещению 1080 на устройстве DRBD) и P10 (полный `dump-md` метаданных `dv-1` в шести состояниях, от только что созданных до полной синхронизации). Перед опытом и после него он убирает ресурсы `p7`–`p10`. Результаты записаны в разделе «Опыты перед CSI (P7–P10)» документа `docs/superpowers/specs/2026-11-12-drbd9-spike-results.md`.

## Проверка агента

Скрипт `stand/agent-smoke.sh` применяет описание тома `smoke` (копии на `dv-1` и
`dv-2`, `dv-3` без диска) командами `ambar` в подах `toolbox`, проверяет списки
действий, состояние DRBD, файлы тома, запись через узел без диска, восстановление
после потери loop-устройства и после отсоединения диска, затем удаляет том.
Перед проверкой нужен образ с агентом: `image/build.sh && stand/k3s.sh`.

```bash
stand/agent-smoke.sh
```

```bash
stand/agent-smoke.sh --reboot
```

Ожидаемая последняя строка обоих запусков: `OK: agent smoke`. С `--reboot` скрипт
дополнительно останавливает `dv-2` через `limactl stop --force dv-2`, запускает
его через `stand/up.sh` и проверяет, что `ambar load-module` загружает модуль, а
`ambar apply` поднимает ресурс. `dv-1` скрипт не останавливает.

Скрипт можно запускать повторно. В начале он убирает остатки прошлого запуска в
таком порядке: размонтирует `/mnt/smoke` на всех узлах, затем выполняет
`ambar remove --name smoke` на всех узлах и проверяет, что `ambar status`
печатает `smoke: not configured`. Размонтирование идёт на всех узлах до первого
`remove`: пока узел без диска держит устройство открытым, `drbdadm down` на
других узлах отказывает (`State change was refused by peer node`). Ошибка
`remove` останавливает скрипт. Если запуск упал на полпути, запустите скрипт
ещё раз или выполните то же вручную:

```bash
bash -c 'source stand/lib.sh
  for n in "${NODES[@]}"; do vm "$n" "if mountpoint -q /mnt/smoke; then umount /mnt/smoke; fi"; done
  for n in "${NODES[@]}"; do tb "$n" "ambar remove --name smoke"; done'
```

Блокировку от одновременных запусков скрипт проверяет на `dv-1`: утилита `flock`
в поде `toolbox` держит `/var/lib/rancher/ambar/volumes/smoke.lock` 6 с, за это
время `ambar apply` должен завершиться с кодом 75, а после освобождения
блокировки с кодом 0. После шага c скрипт также проверяет, что
`blockdev --getsize64 /dev/drbd1010` на `dv-1` не меньше 1073741824.

## Установка драйвера

Скрипт `stand/install.sh` ставит драйвер из `deploy/` на стенд. Перед ним нужны
образ агента и k3s: `image/build.sh && stand/k3s.sh`.

```bash
stand/install.sh
```

Ожидаемая последняя строка: `OK: ambar installed on 3 nodes`. Скрипт выполняет по
порядку:

1. Создаёт на каждом узле `/var/lib/rancher/ambar/volumes` (0700) и
   `/var/lib/rancher/ambar/res`.
2. Удаляет DaemonSet `toolbox` из `ambar-spike` и ждёт завершения его подов: два
   процесса не должны управлять одними ресурсами DRBD.
3. Импортирует образ `IMAGE` в containerd каждого узла. Образы сайдкаров узлы
   тянут сами с `registry.k8s.io`.
4. Применяет `deploy/namespace.yaml`, `deploy/crd.json`, `deploy/csidriver.yaml`,
   `deploy/rbac.yaml`.
5. Создаёт `ConfigMap ambar-nodes` из `NODES`: имя `vm_host`, адрес `vm_ip`,
   `nodeId` по порядку в `NODES`, `capacityBytes` 8 ГиБ.
6. Применяет `deploy/daemonset.yaml` и `deploy/storageclass.yaml` и ждёт
   `rollout status ds/ambar`. Если DaemonSet уже был, перезапускает его, чтобы
   поды взяли пересобранный образ с тем же тегом.
7. Проверяет, что `CSINode` каждого узла содержит драйвер
   `ambar.balookrd.github.io`.

Повторный запуск безопасен. Переменные:

| Переменная | По умолчанию | Что задаёт |
|---|---|---|
| `IMAGE` | `ambar-drbd:dev` | Образ агента; подставляется в `deploy/daemonset.yaml` вместо `AMBAR_IMAGE` |
| `PULL_POLICY` | `Never` | `imagePullPolicy` агента; подставляется вместо `AMBAR_IMAGE_PULL_POLICY` |

На кластере без стенда подставьте оба значения сами (образ должен лежать в
реестре, который видят узлы):

```bash
sed -e 's|AMBAR_IMAGE_PULL_POLICY|IfNotPresent|' \
    -e 's|AMBAR_IMAGE|registry.example.com/ambar-drbd:1.0|' \
    deploy/daemonset.yaml | kubectl apply -f -
```

`ConfigMap ambar-nodes` на таком кластере создаётся по образцу
`deploy/nodes-configmap.example.yaml`.

Удаление:

```bash
stand/install.sh --remove
```

Скрипт удаляет StorageClass, DaemonSet, RBAC, `CSIDriver`, CRD и пространство
имён `ambar-system`, затем возвращает DaemonSet `toolbox`. Если в кластере есть
объекты `ReplicatedVolume`, скрипт ничего не удаляет и завершается с кодом 1,
печатая список объектов: удаление CRD удалило бы их, и агенты увидели бы тома без
объектов. Сначала удалите PVC, которые на них ссылаются.

### Права и сайдкары

Агент работает под `ServiceAccount ambar` без прав `cluster-admin`; права
перечислены в `deploy/rbac.yaml` с указанием места в коде, которому нужно каждое.
Сайдкары: `csi-provisioner` v6.4.0 и `csi-node-driver-registrar` v2.18.0, оба
закреплены по digest в `deploy/daemonset.yaml`. Provisioner запускается в каждом
поде DaemonSet с выбором лидера (Lease `ambar-balookrd-github-io` в
`ambar-system`), активен один.
