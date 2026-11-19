# Стенд ambar

Три ВМ Lima (`dv-1`, `dv-2`, `dv-3`) с Debian 13 trixie arm64, ядром
`6.12.116+deb13-arm64`, k3s из трёх server и привилегированным DaemonSet
`toolbox` с образом `ambar-drbd` (модуль DRBD 9.3.4, `drbd-utils` 9.35.0).
Для проверок `stand/csi-e2e.sh`, `stand/replica-e2e.sh` и `stand/failure-e2e.sh` добавляется четвёртая ВМ `dv-4` (агент k3s, не
server; см. «Стенд из четырёх узлов»).

Префикс `dv` в именах ВМ — историческое сокращение первого названия проекта `duovol` (затем `sklad`, сейчас `ambar`); ВМ не переименованы, потому что для этого Lima-инстанс пришлось бы создавать заново.

Стенд нужен для проверок из `docs/superpowers/specs/2026-11-12-drbd9-spike-results.md`.

## Требования

| Что | Значение |
|---|---|
| Хост | macOS на Apple silicon |
| Lima | 2.3.0, `vmType: vz`, сеть `user-v2` (`stand/lima/node.yaml`) |
| Docker | `docker buildx`, сборка под `linux/arm64` |
| Для `image/build.sh` на хосте | Rust stable 1.100 или новее, `cargo-zigbuild`, `zig`, цель `rustup target add aarch64-unknown-linux-musl` (проверено с zig 0.17.1 и rustc 1.100.0) |
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
| `NODES` | `dv-1 dv-2 dv-3` | Имена ВМ через пробел; читают все скрипты `stand/`. Первые три узла — server k3s, остальные — agent |
| `NET_IF` | `eth0` | Интерфейс общей сети внутри ВМ; с него `vm_ip` берёт адрес узла |
| `KVER` | `6.12.116+deb13-arm64` | Версия ядра, которую проверяют `up.sh` и `check.sh` |
| `IMAGE` | `ambar-drbd:dev` | Образ, который `k3s.sh` импортирует в узлы |
| `K3S_VERSION` | `v1.37.2+k3s1` | Версия k3s для `INSTALL_K3S_VERSION` |

## Стенд из четырёх узлов

`NODES` задаёт список ВМ для `up.sh`, `k3s.sh`, `check.sh`, `install.sh`,
`down.sh`, `csi-e2e.sh`, `replica-e2e.sh` и `failure-e2e.sh`. Первые три имени — server k3s (etcd остаётся из трёх
участников), четвёртое и следующие — agent. Без переменной все скрипты работают
с тремя узлами, как раньше.

```bash
export NODES="dv-1 dv-2 dv-3 dv-4"
stand/up.sh && stand/k3s.sh && stand/check.sh
```

`stand/up.sh` создаёт недостающие ВМ из `stand/lima/node.yaml` (по 2 CPU,
3 ГиБ RAM, 20 ГиБ диска), `stand/k3s.sh` присоединяет `dv-4` к кластеру
командой `k3s agent` с адресом и токеном первого server. Ожидаемые последние
строки: `OK: stand is up` у `up.sh` и `check.sh`, `kc get nodes` показывает
четыре узла `Ready` (`dv-4` без роли `control-plane`). Если драйвер уже
установлен, повторите `stand/install.sh` с четырьмя узлами: он обновляет
`ConfigMap ambar-nodes` (четыре записи, `nodeId` 3 у `dv-4`, остальные
`nodeId` не меняются) и DaemonSet `ambar`: на `dv-4` появляется под агента.

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
`stand/spike/clean.sh`.

## Остановка и удаление стенда

Остановить ВМ, сохранив диски и кластер:

```bash
for n in dv-1 dv-2 dv-3 dv-4; do limactl stop "$n"; done
```

Остановленные ВМ запускает `stand/up.sh` с тем же значением `NODES`.

**Внимание.** `stand/down.sh` не останавливает, а удаляет ВМ вместе с дисками
(`limactl delete --force`). После него стенд создаётся заново: `stand/up.sh`,
`stand/k3s.sh`, `stand/install.sh`. Тома, существовавшие на стенде, теряются.

## Опыты перед CSI

Скрипт `stand/spike/pre-csi.sh <p7|p8|p9|p10|clean>` выполняет опыты P7 (`drbdadm dump-md`), P8 (отключение стороны без диска от работающего ресурса с писателем), P9 (`blkid -p` и чтение по смещению 1080 на устройстве DRBD) и P10 (полный `dump-md` метаданных `dv-1` в шести состояниях, от только что созданных до полной синхронизации). Перед опытом и после него он убирает ресурсы `p7`–`p10`. Результаты записаны в разделе «Опыты перед CSI (P7–P10)» документа `docs/superpowers/specs/2026-11-12-drbd9-spike-results.md`.

## Опыты перед сменой набора копий

Скрипт `stand/spike/pre-2b.sh <p11|p12|p13|p14|clean|toolbox-off>` выполняет опыты P11 (сторона без диска становится стороной с копией, два порядка команд), P12 (уход стороны с копией и стороны без диска), P13 (слоты битовой карты при смене сторон, `--max-peers=1`, с `forget-peer` и без него) и P14 (кворум `dv-1`, пока третья копия `Inconsistent`, со свидетелем и без него). Драйвер на стенде считает своим любой `<имя>.img`, `<имя>.res` и `/dev/ambar/<имя>` (`src/agent/local.rs`), поэтому скрипт держит файлы опытов в `/var/lib/rancher/ambar-exp/{volumes,res}` и `/dev/ambar-exp` и сам создаёт DaemonSet `toolbox` в `ambar-spike`, который монтирует эти каталоги; DaemonSet, CRD и пространство имён драйвера не затрагиваются. Перед опытом и после него скрипт убирает ресурсы `p11`–`p14`; `toolbox-off` удаляет DaemonSet `toolbox`. Результаты записаны в разделе «Опыты перед сменой набора копий (P11–P14)» документа `docs/superpowers/specs/2026-11-12-drbd9-spike-results.md`.

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

Ожидаемая последняя строка: `OK: ambar installed on 3 nodes` (`4 nodes` при
`NODES` из четырёх узлов). Скрипт выполняет по
порядку:

1. Создаёт на каждом узле `/var/lib/rancher/ambar/volumes` (0700) и
   `/var/lib/rancher/ambar/res`.
2. Удаляет DaemonSet `toolbox` из `ambar-spike` и ждёт завершения его подов: два
   процесса не должны управлять одними ресурсами DRBD.
3. Импортирует образ `IMAGE` в containerd каждого узла. Образы сайдкаров узлы
   тянут сами с `registry.k8s.io`.
4. Применяет `deploy/namespace.yaml`, `deploy/crd.json`, `deploy/csidriver.yaml`,
   `deploy/rbac.yaml`.
5. Создаёт или дополняет `ConfigMap ambar-nodes` (на хосте нужен `python3`).
   Имя узла — `vm_host`, адрес — `vm_ip`, `capacityBytes` 8 ГиБ. `nodeId`
   узла никогда не меняется: существующая запись (имя, адрес, `nodeId`,
   `capacityBytes`) переносится как есть, новому узлу достаётся наименьший
   свободный `nodeId`. Если узел из ConfigMap отсутствует в `NODES` или его
   адрес изменился, скрипт завершается с кодом 1 и сообщением
   `FAIL: node <имя> …`, ConfigMap остаётся прежним.
6. Применяет `deploy/daemonset.yaml` и `deploy/storageclass.yaml` и ждёт
   `rollout status ds/ambar` до 300 с. В шаблон пода добавляется аннотация
   `ambar.balookrd.github.io/image-id` с ID образа `IMAGE` из `docker image
   inspect`: пересобранный образ с тем же тегом меняет шаблон, и DaemonSet
   заменяет поды по одному.
7. Проверяет, что `CSINode` каждого узла содержит драйвер
   `ambar.balookrd.github.io`.
8. Считает на каждом узле монтирования `/var/lib/kubelet/plugins/ambar` в
   `/proc/self/mountinfo`; если их больше одного, печатает в stderr
   `WARN: …` с командой `limactl shell <узел> sudo umount -R -l
   /var/lib/kubelet/plugins/ambar`. Скрипт эту команду не выполняет.

Повторный запуск с теми же узлами: `kubectl apply` печатает `unchanged` для
всех объектов, `ConfigMap ambar-nodes` остаётся побайтно прежним (та же
`resourceVersion`), `metadata.generation` DaemonSet и поды не меняются; порядок
узлов в `NODES` на `nodeId` не влияет. Поды заменяются, только если изменился
шаблон DaemonSet (манифест или ID образа). Переменные:

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

## Сквозная проверка CSI

Скрипт `stand/csi-e2e.sh` проверяет драйвер через PVC и поды. Нужны четыре узла и
установленный драйвер: четвёртый узел нужен шагам e и f (членство `Client`
появляется, когда под попадает на узел, который не входит в `spec.nodes` тома);
при трёх узлах скрипт сразу завершается с кодом 2.

```bash
export NODES="dv-1 dv-2 dv-3 dv-4"
stand/install.sh && stand/csi-e2e.sh
```

Ожидаемая последняя строка: `OK: csi e2e`. Код возврата 0 только при успехе всех
шагов. При ошибке скрипт печатает `FAIL: [step <шаг>] …`, затем
`kc get rv -o yaml`, поды и PVC, события пространства имён `default` и хвост
журнала каждого агента и завершается с кодом 1.

| Шаг | Что проверяет |
|---|---|
| a | PVC `e2e-r3` (`ambar-r3`) становится `Bound` за ≤ 120 с, под `w3` на `dv-1` пишет файл; у `ReplicatedVolume` три `Replica`, `initialSync: Done`, три записи `status.nodes` с `disk: UpToDate` |
| b | `blockdev --getsize64 /dev/drbd<minor>` на `dv-1` не меньше 1073741824 |
| c | Под `r3` на `dv-2` читает файл, записанный с `dv-1`; на `dv-1` роль `Secondary` |
| c2 | Наблюдение (без проверок результата): второй под с тем же PVC на другом узле, события, `drbdsetup status`, судьба записи `Client` |
| d | PVC `e2e-r2` (`ambar-r2`) и под на `dv-1`: две `Replica`, один `Witness`, четвёртого узла в `spec.nodes` нет |
| e | Под на узле вне `spec.nodes`: появляется запись `Client`, данные читаются, файла `<том>.img` на узле нет |
| f | После удаления пода запись `Client` исчезает за ≤ 60 с, на узле нет ресурса DRBD, у остальных нет соединения с ним |
| g | Под на узле `Witness`: `spec.nodes` не меняется, данные читаются |
| h | `rollout restart` DaemonSet `ambar` под писателем со счётчиком (`dd conv=fsync`): счётчик не уменьшается и растёт после перезапуска; испорченная запись `status.nodes` возвращается агентом |
| i | StorageClass с `replicas: "1"`: PVC `Pending`, в событии `InvalidArgument` |
| j | После удаления подов и PVC объектов `ReplicatedVolume` нет за ≤ 120 с, на узлах нет файлов, loop-устройств и ресурсов DRBD |
| k | Объект удалён, пока агент узла с копией не работал: у остальных узлов финализаторов и файлов тома нет, остался финализатор узла без агента (его снимает `kubectl patch`); после возврата агента в журнале две строки уровня error с именем тома, файл `<том>.img` остался; остатки убирает `ambar remove --name <том>` |

Образ тестовых подов: `busybox` 1.37.0, закреплён по digest в переменной
`E2E_IMAGE` скрипта. Шаг k временно убирает агент с узла: в `DaemonSet ambar`
добавляется `nodeAffinity` с условием `ambar-e2e-skip DoesNotExist` (шаблон
меняется, агенты перезапускаются один раз), узел получает метку
`ambar-e2e-skip=true`; в конце шага (при ошибке и по Ctrl-C — через trap на
`EXIT`) метка и `nodeAffinity` снимаются. Имя тома шага k записывается в
`ConfigMap ambar-e2e-state` пространства имён `default` до удаления объекта;
если шаг прерван, следующий запуск выполняет `ambar remove --name <имя>` для
этого имени (имя должно иметь вид `pvc-<uuid>`) и удаляет ConfigMap. Других
остатков скрипт не убирает. Наблюдавшиеся времена и разбор запусков: раздел «Проверки CSI на
стенде» в `docs/superpowers/specs/2026-11-12-drbd9-spike-results.md`.

## Сквозная проверка смены набора копий

Скрипт `stand/replica-e2e.sh` проверяет смену набора копий тома (`ambar replica add`, `remove`, `move`) под работающим писателем, переназначение свидетеля, таймаут ожидания и правила CRD. Нужны четыре узла и установленный драйвер.

```bash
export NODES="dv-1 dv-2 dv-3 dv-4"
stand/install.sh && stand/replica-e2e.sh
```

Ожидаемая последняя строка: `OK: replica e2e`.

| Шаг | Что проверяет |
|---|---|
| a | PVC `replica-vol` (`ambar-r2`) и под-писатель `writer` на `dv-1`: у тома две `Replica`, один `Witness`; писатель пишет номер строки со счётчиком и `fsync` каждую секунду |
| b | `replica add <том> <узел-свидетель>`: код 0; три `Replica`, свидетеля нет, финализатор добавлен, файл на новом узле есть; под не перезапускался; счётчик в логе писателя растёт без пропусков > 5 с |
| c | `replica remove <том> <узел, не dv-1>`: код 0; две `Replica` и новый `Witness`; на снятом узле нет файла тома, файла ресурса, ссылки и ресурса в DRBD; финализатор узла снят |
| d | `replica remove <том> dv-1` (под там): код 1, текст ошибки о `Primary`; объект `spec.nodes` не изменился |
| e | `replica move <том> <копия не на dv-1> <узел вне тома>`: код 0; копии на `dv-1` и на целевом узле, свидетель остаётся прежним (N чётно до и после); ровно две строки `edit:`. Затем перенос на узел свидетеля: назначен другой свидетель |
| f | `replica move … --timeout 20` на 2-ГиБ томе с 1.5 ГиБ данных: код 1 во время синхронизации, в контейнере агента нет зависших процессов `ambar replica`; затем тот же `move` без `--timeout` доводит перенос до конца; сумма строк `edit:` обоих запусков — 2 |
| g | Правила валидации CRD через `kubectl patch --type=json`: отказ при удалении `Replica` напрямую, при смене `Replica -> Client`, при добавлении `Retiring` извне и при двух `Retiring` |
| h | Писатель останавливается, под `reader` проверяет, что последнее подтверждённое значение счётчика есть на диске после `sync`; удаление PVC удаляет файлы и ресурсы на всех 4 узлах |
| i | `grep -ci forbidden` по журналам всех контейнеров четырёх подов агента равен 0; число перезапусков контейнеров не изменилось |

## Сквозная проверка обработки отказов (Фаза 3)

Скрипт `stand/failure-e2e.sh` проверяет реакцию кластера и драйвера на отказы сети и узлов: изоляцию сети репликации через `nftables`, выставление taint `no-quorum:NoSchedule` (R32), выселение подов по истечении 30 с grace period (R33), авто-восстановление соединений `StandAlone` в роли Secondary через `adjust` (R34), фиксацию метрики split-brain `manual_check_needed` (R35), автоматическую очистку забытого узла `Client` без подов (R36), а также работу HTTP-сервера метрик на порту 9770 (R38). Нужны четыре узла и установленный драйвер.

```bash
export NODES="dv-1 dv-2 dv-3 dv-4"
stand/install.sh && stand/failure-e2e.sh
```

Ожидаемая последняя строка: `OK: all failure handling e2e tests passed cleanly!`.

| Шаг | Что проверяет |
|---|---|
| a | HTTP-сервер метрик на порту 9770: `/metrics` отдаёт валидный формат Prometheus со всеми метриками (`ambar_node_allocated_bytes`, `ambar_node_tainted`, `ambar_module_info` и др.); некорректный путь возвращает 404 |
| b | Создание PVC `fail-pvc` (`ambar-r2`) и Deployment `fail-writer` с непрерывной записью heartbeat: том в состоянии Primary, `quorum: true`, метрика `ambar_volume_quorum=1`, taint отсутствует |
| c | Изоляция трафика репликации узла писателя через `nftables`: DRBD фиксирует потерю кворума (`quorum: false`); агент выставляет taint `ambar.balookrd.github.io/no-quorum:NoSchedule`; метрика `ambar_node_tainted=1`, `ambar_volume_quorum=0` |
| d | Grace period 30 с: под не выселяется первые 20 с; по истечении 30 с агент выселяет под через Kubernetes API; Deployment перезапускает под на выжившем узле с кворумом (`dv-1`); запись heartbeat продолжается |
| e | Восстановление сети (снятие правил `nftables`): размонтированный том на изолированном узле в роли `Secondary` видит `StandAlone`; агент автоматически запускает `adjust`, соединение возвращается в `Connected`, диск ресинхронизируется в `UpToDate`; taint `no-quorum` автоматически снимается; метрика `ambar_node_tainted=0`, split-brain метрика `0` |
| f | Очистка Client без подов: запуск читающего пода на узле без реплики (`dv-4`) переводит его в `Client`; после удаления пода агент фиксирует отсутствие подов на узле, выполняет `drbdadm down` и удаляет узел из `spec.nodes` и `status.nodes` |
| g | Удаление тома и чистота стенда: PVC удалён, ReplicatedVolume объект удалён; на всех 4 узлах отсутствуют файлы томов, loop-устройства, правила nftables и taints; счётчик рестартов контейнеров ambar не изменился |


