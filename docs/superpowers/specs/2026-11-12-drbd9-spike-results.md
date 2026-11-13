# Результаты проверки допущений DRBD 9 на стенде

Стенд: три ВМ Lima на маке, Debian 13 trixie arm64, ядро
`6.12.116+deb13-arm64`, DRBD 9.3.4, drbd-utils 9.35.0. Каждое допущение взято
из раздела «Что проверяется первым» спеки
`2026-11-12-ambar-design.md`.

Опыты выполнены, когда проект и его пути назывались `duovol` (затем проект назывался `sklad`); имена в выводах
позже заменены на текущее имя `ambar` (например, `/var/lib/rancher/ambar/...`, `ambar-spike`).

## Окружение

| Что | Значение |
|---|---|
| Версия Lima | `limactl version 2.3.0`, тип `vz`; на узел 2 CPU (`nproc` = 2), 2911 МиБ `MemTotal` по `free -m` |
| Версия k3s | `k3s version v1.37.2+k3s1 (f6973cb8)` на трёх узлах, все три — `control-plane,etcd` |
| Интерфейс общей сети на узлах | `eth0`, 192.168.104.1 / .3 / .4 (`dv-1` / `dv-2` / `dv-3`) |
| Имена узлов (`uname -n`) | `lima-dv-1`, `lima-dv-2`, `lima-dv-3`; `node-id` 0, 1, 2 |
| Порты ресурсов | `spike`: 7000, minor 1000; `spike2`: 7001, minor 1001 |
| Сетевые таймауты DRBD | по умолчанию: `ping-int 10` с, `ping-timeout 0.5` с, `timeout 6` с, `connect-int 10` с, `ko-count 7` |

## Итог по допущениям

| № | Допущение | Результат | Что меняется в спеке |
|---|---|---|---|
| 1 | Модуль 9.3.4 загружается с `usermode_helper=disabled` | подтверждено предыдущей задачей: модуль 9.3.4 загружен на трёх узлах, `/sys/module/drbd/parameters/usermode_helper` = `disabled`, в `dmesg` только предупреждения о модуле вне дерева ядра и без подписи; после перезапуска `dv-2` `load-module.sh` загрузил его снова (шаг 8) | ничего |
| 2 | Ресурс из трёх сторон поверх loop с `--direct-io=on` поднимается | подтверждено: `res.sh` без правок, `DIO` = `1` на трёх узлах, три стороны `Connected`; досылки после изоляции и остановки узла идут по битовой карте (32 КиБ; 16668 КиБ за ~30 с под нагрузкой писателя; 16520 КиБ за 1 с без нагрузки, E2) | «Поведение при отказах», строка «Узел вернулся»: последовательность состояний диска по каждому наблюдавшемуся случаю (шаги 6, 8, E1, E2) |
| 3 | Начальная синхронизация пропускается через `new-current-uuid --clear-bitmap` | подтверждено: одна команда `drbdadm new-current-uuid --clear-bitmap <имя>/0` на `dv-1` после соединения всех сторон перевела все копии в `UpToDate` за < 4 с без досылки; так же для ресурса со стороной без диска | «Создание (`CreateVolume`)»: пометка `[проверить на стенде: …]` заменена наблюдаемой последовательностью |
| 4 | Сторона без диска участвует в кворуме и открывает устройство на запись | подтверждено для стороны, у которой кворум уже был: `dv-3` без диска в роли `Primary` пишет; при штатной остановке `dv-2` (шаг 8) и при принудительной (`limactl stop --force`, E2) кворум не потерян (`dv-3` + `dv-1`; на `dv-1` — `Would lose quorum, but using tiebreaker logic to keep`), запись стоит 4.6–5.6 с (5.1 с между выборками в обоих опытах) до `PingAck did not arrive in time` и продолжается; по одному прогону на каждый вид остановки. Заново запущенной устаревшей копии узел без диска кворума не дал (C1, два прогона: `1 of 2 nodes visible, need 2 for quorum`); объяснение различия — `[проверить: tiebreaker сохраняет уже имевшийся кворум, но не даёт его заново запущенному узлу — предположение]` | «Поведение при отказах», строка «Упал узел без пода»: пауза записи, вид остановки, сообщение о tiebreaker |
| 5 | `on-no-quorum`: `io-error` или `suspend-io` | выбран `io-error` (по одному прогону каждого режима). `io-error`: изолированный узел через 10–11 с получает `EIO` от `fsync`. `io-error`: если том размонтирован до возврата сети, `umount` проходит и узел уходит в `Secondary`. `suspend-io`: `sync` висит в `D` до возврата сети (77 с). `suspend-io`: `SIGKILL` не действует, `umount` → `target is busy`, устройство открыто, узел остаётся `Primary`; освободить том на изолированном узле не удалось. `StandAlone` после возврата сети зависит от роли узла в этот момент, не от политики: в шаге 6 (`Secondary`) его нет, в опыте E1 (`io-error`, том не размонтирован, `Primary`) обе связи `dv-1` перешли в `StandAlone` с `I shall become SyncTarget, but I am primary!`. Восстановление в E1: размонтирование и `drbdadm adjust` на трёх узлах. Подтверждённые записи на месте во всех опытах. `suspend-io` — значение по умолчанию в 9.3.4 | «Параметры ресурса DRBD»: `io-error` закреплён, строка в `.res` обязательна, описание ext4 по наблюдению; абзац о выборе разделён на политику и роль при возврате; «Поведение при отказах», строка «Узел с подом изолирован по `lan0`»: время, текст `dmesg`, `StandAlone` при возврате в роли `Primary`; абзац о split-brain: пометка `[проверить: …]`; «Монтирование»: код ошибки при открытии на втором узле |
| 6 | `drbdadm` и `drbdsetup` работают из контейнера | подтверждено предыдущей задачей: в поде `drbdadm --version` → `DRBD_KERNEL_VERSION=9.3.4`, `DRBDADM_VERSION=9.35.0`; в этой проверке все команды `drbdadm`/`drbdsetup` (`create-md`, `up`, `adjust`, `new-current-uuid`, `status --json`, `secondary`, `down`) выполнялись в поде `toolbox` | «Утилиты DRBD»: пометка о версии `drbd-utils` заменена на «проверено с 9.35.0» |

## Итог по опытам C1–C3

По одному прогону на каждый случай. Подробности, команды и выводы — в
разделах «Опыт C1», «Опыт C2», «Опыт C3» в конце протокола.

| Опыт | Вопрос | Результат |
|---|---|---|
| C1, вариант A | Устаревшая копия `dv-2` и узел без диска `dv-3` (не перезапускался) поднимаются при выключенной новейшей копии `dv-1`: получают ли кворум, принимают ли запись | нет: `quorum:no` на обоих; `mount` (rw) и `drbdadm primary` отклонены на обоих (`dv-3`: `Need access to UpToDate data`, `dv-2`: `No quorum`, `1 of 2 nodes visible, need 2 for quorum`); `mount -o ro,noload` выполнялся только на `dv-2` (отказ, причина не записана); запись не пробовали, `primary --force` не выполнялся; `dv-3` понизил `dv-2` до `Consistent` (`Downgrading joining peer's disk as its data is older`). После возврата `dv-1` — `SyncSource`, split-brain нет; на томе 9349 при ACKED 9348 |
| C1, вариант B | То же, но `dv-3` тоже перезапущен и не помнит новых данных | нет: `quorum:no` на обоих, хотя `dv-3` видит `dv-2` как `UpToDate`; `mount`/`primary` отклонены с `No quorum` (`dv-3`: `no UpToDate peer with quorum`). После возврата `dv-1` — `SyncSource`, split-brain нет; на томе 12852 при ACKED 12851 |
| C1, `quorum-minimum-redundancy` | Знают ли опцию drbd-utils 9.35.0 / модуль 9.3.4 | опция есть (`drbdsetup help resource-options`, `--show-defaults`: `quorum-minimum-redundancy off; # default`); шаг 5 не выполнялся, потому что ни A, ни B не дали повысить устаревшую копию |
| C2, прогон 1 | Изолированный `Primary` с `EIO`, пиры не продвинулись: восстанавливаются ли связи сами | да, через ~2 с после снятия правил `nft`, `dv-1` остаётся `Primary` и становится `SyncSource` (8 КиБ); ext4 на `dv-1` остаётся с прерванным журналом (`EIO` на запись и чтение) до `umount` + `mount` |
| C2, прогон 2 | Пиры продвинулись (запись на `dv-2`); помогает ли `adjust` без размонтирования | нет: `adjust` на трёх узлах (`rc=0`) приводит к новой попытке соединения с тем же `I shall become SyncTarget, but I am primary!` и снова к `StandAlone`; после `umount` на `dv-1` (авто-`Secondary`) `adjust` на трёх узлах соединил всех за ~2 с |
| C3 | Как выглядит настоящий split-brain (`quorum off`, запись на двух сторонах) | ядро: `uuid_compare()=split-brain-auto-recover by rule=bitmap-both` и `Split-Brain detected but unresolved, dropping connection!`; в `drbdsetup status --json` нет поля, которым состояние отличалось бы от `StandAlone` из E1 (набор ключей совпадает); в захвате `events2` опыта C3 отличающего признака нет, `events2` для E1 не снимался; разрешено `disconnect` + `connect --discard-my-data` на `dv-1` и `connect` на двух других узлах. Записи `dv-1` 897–15875, подтверждённые писателем после изоляции, потеряны. Это следствие запуска с `quorum off`. При `quorum majority` (проектная конфигурация) split-brain не получен и не проверялся; единственное свидетельство о `quorum majority` — E1: изолированный `Primary` с `quorum:no` получает `EIO`, подтверждённые данные целы |

## Отклонения от брифа

| Место в брифе | Что сказано | Что наблюдалось и что сделано |
|---|---|---|
| Шаг 4, ожидание `drbdadm status` | оба соединения `Connected` | `drbdadm status` не печатает `connection:Connected`; состояние видно в `drbdsetup status --verbose` и в `--json` (`connection-state`) |
| Шаг 6, `pgrep -f probe.sh \|\| echo 'probe exited'` | признак завершения писателя | шаблон находит обёртку `sudo bash -c "… probe.sh …"`, проверка всегда «жив»; использованы `ps … \| grep '[p]robe.sh'` и `stand/spike/watch.sh` |
| Шаг 7, `drbdsetup show spike \| grep on-no-quorum` | ожидается `on-no-quorum suspend-io;` | пусто: `suspend-io` — значение по умолчанию, `show` его не печатает; проверено через `--show-defaults` |
| Шаг 7, `cat /proc/$(pgrep -f probe.sh)/status` | состояние писателя | показывает `bash` (`S`); зависший процесс — дочерний `sync` в `D`, найден через `pgrep -x sync` |
| Шаг 7, `timeout 20 umount` | `rc=124` — признак зависания | `umount` не висит, а сразу возвращает `target is busy` (`rc=32`) |
| Шаг 8, `pgrep -x probe.sh` | признак «писатель жив» | имя процесса `bash`, проверка всегда ложна; живость видна по росту номера и `watch.sh` |
| Шаг 6–8, интервалы | не заданы | добавлен `stand/spike/watch.sh`: время, номер, состояние писателя раз в 0.5 с |
| Шаг 7, повтор шага 6 | без отличий | после проверки `umount` добавлены `kill -9` зависшего `sync` и `drbdadm secondary`, чтобы проверить, освобождается ли устройство |
| `clean.sh` первой версии | — | `pkill -x watch.sh` не находил `watch.sh` (имя процесса `bash`), два `watch.sh` оставались на `dv-1` и `dv-3`; шаблоны заменены на `pkill -f '^bash /usr/local/bin/(probe\|watch)\.sh'`, в `watch.sh` шаблон поиска писателя тоже закреплён `^` |
| После ревью | — | добавлены опыты E1 (`io-error`, узел `Primary` при возврате сети) и E2 (принудительная остановка `dv-2`) |
| C1, команды DRBD при двух выключенных ВМ | через контейнер toolbox без API | добавлен `stand/spike/ctr.sh <узел> <команда>`: находит контейнер `k3s crictl ps --name toolbox --state running -q` и выполняет команду через `k3s crictl exec -i … bash -s`; проверен до выключения ВМ; в опытах C1–C3 все команды `drbdadm`/`drbdsetup` шли через него |
| C1, вариант B, `limactl start --tty=false dv-3` при двух выключенных ВМ | запуск ВМ | узел не дошёл до `READY` за 300 с (`timeout 300`, `rc=124`), Lima ждала `User session to be ready for SSH`; ВМ при этом работала. Снимок `systemctl list-jobs` в госте (через ~3 мин после старта): `k3s.service start running`, `multi-user.target` и `cloud-final.service` в `waiting`. `journalctl -u k3s` и журнал etcd не снимались; `[проверить: причина — ожидание кворума etcd]`. После `limactl start dv-2` (два узла из трёх) оба узла дошли до `running` |
| C3, `drbdadm disconnect spike` на жертве | первый шаг разрешения | ядро отвечает `State change failed: Can not disconnect a StandAlone device (-9)`, `drbdadm` возвращает `rc=0` |
| C3, `drbdadm connect spike` на выживших | соединить с жертвой | `rc=10`, `Failure: (125) Device has a net-config (use disconnect first)` для уже соединённого пира; соединение с `StandAlone`-пиром при этом поднялось |

## Протокол

Команды на маке выполнялись из корня репозитория через `bash -c 'source stand/lib.sh; …'`.
`vm` запускает команду от root на узле, `tb` — в поде `toolbox` этого узла.
Отметки времени `date +%T` и счётчики `[ N ]` в `dmesg` у каждого узла свои:
время `dmesg` отсчитывается от загрузки узла. Внутри пода `toolbox` часы
показывают UTC (на узлах +08).

### Шаг 4. Ресурс `spike` из трёх копий

```bash
stand/spike/res.sh spike 1000 7000 - io-error
```

Скрипт отработал без правок. Хвост вывода (`create-md` на трёх узлах и
`drbdadm status spike` на `dv-1`):

```text
initializing bitmap (64 KB) to all zero
initializing activity log
Writing meta data...
New drbd meta data block successfully created.
...
spike role:Secondary
  disk:Inconsistent open:no
  lima-dv-2 role:Secondary
    peer-disk:Inconsistent
  lima-dv-3 role:Secondary
    peer-disk:Inconsistent
```

`drbdadm status` не печатает `connection:Connected`, если соединение
установлено: строка соединения есть только при отличном от `Connected`
состоянии. Полный вид даёт `drbdsetup status spike --verbose --statistics`:

```text
spike node-id:0 role:Secondary suspended:no force-io-failures:no
    write-ordering:flush
  volume:0 minor:1000 disk:Inconsistent backing_dev:/dev/ambar/spike quorum:yes
      open:no
      size:1048476 read:0 written:0 al-writes:0 bm-writes:0 upper-pending:0
      lower-pending:0 al-suspended:no blocked:no
  lima-dv-2 node-id:1 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:Inconsistent resync-suspended:no
        received:0 sent:0 out-of-sync:1048476 pending:0 unacked:0
  lima-dv-3 node-id:2 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:Inconsistent resync-suspended:no
        received:0 sent:0 out-of-sync:1048476 pending:0 unacked:0
```

`size:1048476` в КиБ: из файла 1 ГиБ (1048576 КиБ) внутренние метаданные
для двух пиров заняли 100 КиБ, из них 64 КиБ — битовая карта (вывод
`create-md`). Остальные 36 КиБ по выводу утилит не разложены. Сгенерированный файл `/var/lib/rancher/ambar/res/spike.res`:

```text
resource spike {
    options { quorum majority; on-no-quorum io-error; auto-promote yes; }
    net { protocol C; }
    on lima-dv-1 {
        node-id 0;
        address ipv4 192.168.104.1:7000;
        volume 0 { device minor 1000; disk /dev/ambar/spike; meta-disk internal; }
    }
    on lima-dv-2 {
        node-id 1;
        address ipv4 192.168.104.3:7000;
        volume 0 { device minor 1000; disk /dev/ambar/spike; meta-disk internal; }
    }
    on lima-dv-3 {
        node-id 2;
        address ipv4 192.168.104.4:7000;
        volume 0 { device minor 1000; disk /dev/ambar/spike; meta-disk internal; }
    }
    connection-mesh { hosts lima-dv-1 lima-dv-2 lima-dv-3; }
}
```

`drbdadm dump spike` в поде разбирает файл без ошибок и предупреждений.

Прямой ввод-вывод loop-устройства, одинаково на трёх узлах:

```bash
vm dv-1 "losetup -l -O NAME,BACK-FILE,DIO | grep spike"
```

```text
/dev/loop0 /var/lib/rancher/ambar/volumes/spike.img   1
```

Пропуск начальной синхронизации:

```bash
tb dv-1 "drbdadm new-current-uuid --clear-bitmap spike/0"; echo rc=$?
sleep 3; tb <узел> "drbdadm status spike"   # на трёх узлах
```

```text
21:32:52
rc=0
21:32:56
== dv-1
spike role:Secondary
  disk:UpToDate open:no
  lima-dv-2 role:Secondary
    peer-disk:UpToDate
  lima-dv-3 role:Secondary
    peer-disk:UpToDate
== dv-2
spike role:Secondary
  disk:UpToDate open:no
  lima-dv-1 role:Secondary
    peer-disk:UpToDate
  lima-dv-3 role:Secondary
    peer-disk:UpToDate
== dv-3
spike role:Secondary
  disk:UpToDate open:no
  lima-dv-1 role:Secondary
    peer-disk:UpToDate
  lima-dv-2 role:Secondary
    peer-disk:UpToDate
```

Запасной вариант (`primary --force`) не понадобился. `dmesg` на `dv-1`:

```text
[ 1415.765835] drbd spike/0 drbd1000 lima-dv-3: uuid_compare()=no-sync by rule=just-created-both
[ 1435.969824] drbd spike/0 drbd1000 lima-dv-3: Preparing to skip initial sync
[ 1435.969849] drbd spike/0 drbd1000 lima-dv-2: Preparing to skip initial sync
[ 1435.969856] drbd spike/0 drbd1000: Bitmap UUID for initial resync: 6FBF56FB3A46B944
[ 1435.971401] drbd spike/0 drbd1000: new current UUID: 02BE5916891BC44C weak: FFFFFFFFFFFFFFF9
[ 1435.971654] drbd spike/0 drbd1000 lima-dv-3: cleared bitmap UUID 02BE5916891BC44C:0000000000000000:0000000000000000:0000000000000000
[ 1435.971670] drbd spike/0 drbd1000 lima-dv-2: cleared bitmap UUID 02BE5916891BC44C:0000000000000000:0000000000000000:0000000000000000
[ 1435.971692] drbd spike/0 drbd1000: disk( Inconsistent -> UpToDate ) [new-c-uuid]
[ 1435.971695] drbd spike/0 drbd1000 lima-dv-3: pdsk( Inconsistent -> UpToDate ) replication( no -> yes ) [new-c-uuid]
[ 1435.971697] drbd spike/0 drbd1000 lima-dv-2: pdsk( Inconsistent -> UpToDate ) replication( no -> yes ) [new-c-uuid]
```

`dmesg` на `dv-2` (пир):

```text
[ 1383.104098] drbd spike/0 drbd1000: Accepted new current UUID, preparing to skip initial sync
[ 1383.104471] drbd spike/0 drbd1000: disk( Inconsistent -> UpToDate ) [skip-initial-sync]
[ 1383.104475] drbd spike/0 drbd1000 lima-dv-1: pdsk( Inconsistent -> UpToDate ) replication( no -> yes ) [skip-initial-sync]
```

Одна команда на одном узле перевела в `UpToDate` все три копии; запуска
досылки (`SyncSource`/`SyncTarget`) в `dmesg` нет. Команда выполнялась, когда
все три стороны уже были `Connected`.

Образцы `drbdsetup status spike --json` до и после команды сохранены в
`stand/spike/samples/three-inconsistent.json` и
`stand/spike/samples/three-uptodate.json`. Вывод до команды (второе
соединение сокращено):

```json
[
{
  "name": "spike",
  "node-id": 0,
  "role": "Secondary",
  "suspended": false,
  "suspended-user": false,
  "suspended-no-data": false,
  "suspended-fencing": false,
  "suspended-quorum": false,
  "force-io-failures": false,
  "write-ordering": "flush",
  "devices": [
    {
      "volume": 0,
      "minor": 1000,
      "disk-state": "Inconsistent",
      "client": false,
      "open": false,
      "quorum": true,
      "size": 1048476,
      "read": 0,
      "written": 0,
      "al-writes": 0,
      "bm-writes": 0,
      "upper-pending": 0,
      "lower-pending": 0
    } ],
  "connections": [
    {
      "peer-node-id": 1,
      "name": "lima-dv-2",
      "connection-state": "Connected", 
      "congested": false,
      "peer-role": "Secondary",
      "tls": false,
      "ap-in-flight": 0,
      "rs-in-flight": 0,
      "paths": [
        {
          "this_host": {
            "address": "192.168.104.1",
            "port": 7000,
            "family": "ipv4"
          },
          "remote_host": {
            "address": "192.168.104.3",
            "port": 7000,
            "family": "ipv4"
          },
          "established": true
        } ],
      "peer_devices": [
        {
          "volume": 0,
          "replication-state": "Established",
          "peer-disk-state": "Inconsistent",
          "peer-client": false,
          "resync-suspended": "no",
          "received": 0,
          "sent": 0,
          "out-of-sync": 1048476,
          "pending": 0,
          "unacked": 0,
          "has-sync-details": false,
          "has-online-verify-details": false,
          "percent-in-sync": 0.00
        } ]
    },
    {
      "peer-node-id": 2,
      "name": "lima-dv-3",
      "...": "второе соединение той же формы, remote_host 192.168.104.4"
    } ]
}
]
```

Последний элемент в блоке выше сокращён вручную (ключ `"..."`); полный вывод —
в файле образца. Наблюдения по формату для разбора:

| Поле | Наблюдение |
|---|---|
| Верхний уровень | массив ресурсов, даже при запросе одного ресурса |
| Имена ключей | смесь `kebab-case` (`disk-state`, `peer-node-id`) и `snake_case` (`peer_devices`, `this_host`, `remote_host`) |
| `resync-suspended` | строка, а не `bool`: в образцах `"no"` и `"dependency"` (`resync-target.json`) |
| `percent-in-sync` | число с двумя знаками (`0.00`, `100.00`) |
| `size`, `out-of-sync`, `received`, `sent` | КиБ |
| `devices[].quorum` | `bool` на уровне устройства, на уровне ресурса поля `quorum` нет |
| `devices[].client` / `peer_devices[].peer-client` | `true` у стороны без диска (см. шаг 8) |

### Шаг 5. `auto-promote` и запрет второго открытия

```bash
tb dv-1 "mkfs.ext4 -q /dev/drbd1000"; echo mkfs rc=$?
vm dv-1 "mkdir -p /mnt/spike && mount /dev/drbd1000 /mnt/spike"; echo mount1 rc=$?
tb dv-1 "drbdadm status spike | head -2"
vm dv-2 "mkdir -p /mnt/spike && mount /dev/drbd1000 /mnt/spike"; echo "rc=$?"
vm dv-2 "dmesg | tail -4"
vm dv-2 "mount -o ro /dev/drbd1000 /mnt/spike"; echo "ro rc=$?"
vm dv-2 "exec 3</dev/drbd1000 && echo opened-ro-fd"; echo "openro rc=$?"
```

```text
mkfs rc=0
mount1 rc=0
spike role:Primary
  disk:UpToDate open:yes
mount: /mnt/spike: fsconfig() failed: /dev/drbd1000: Can't open blockdev.
       dmesg(1) may have more information after failed mount system call.
rc=32
[ 1403.615630] drbd spike lima-dv-1: Committing remote state change 3757591886 (primary_nodes=1)
[ 1403.615648] drbd spike lima-dv-1: peer( Secondary -> Primary ) [remote]
[ 1406.263087] drbd spike: Auto-promote failed: Multiple primaries not allowed by config (-1)
[ 1415.895015] drbd spike: Auto-promote failed: Multiple primaries not allowed by config (-1)
mount: /mnt/spike: fsconfig() failed: /dev/drbd1000: Can't open blockdev.
       dmesg(1) may have more information after failed mount system call.
ro rc=32
bash: line 1: /dev/drbd1000: Wrong medium type
openro rc=1
```

Второе монтирование отвергнуто: `mount` завершается с кодом 32 и текстом
`Can't open blockdev`, ядро пишет `Auto-promote failed: Multiple primaries not
allowed by config (-1)`. Даже открытие устройства только на чтение на `dv-2`
возвращает `EMEDIUMTYPE` (`Wrong medium type`), пока пир в роли `Primary`.
Признак «том открыт на другом узле» для агента: `open(2)` на
`/dev/drbd<minor>` возвращает `EMEDIUMTYPE`; текст `mount` его не содержит.

Файловая система создана `mkfs.ext4` из образа `toolbox` с поведением при
ошибках по умолчанию:

```text
Default mount options:    user_xattr acl
Errors behavior:          Continue
```

Образец `stand/spike/samples/primary-mounted.json` снят на `dv-1` после
монтирования: `"role": "Primary"`, `"open": true`, у обоих пиров
`"peer-role": "Secondary"`.

### Шаг 6. Изоляция узла с писателем, `on-no-quorum io-error`

Кроме писателя `probe.sh` на `dv-1` запущен `stand/spike/watch.sh`: дважды в
секунду пишет время, последнее подтверждённое число и состояние процесса
писателя. По нему считаются интервалы. Метки времени `date +%T.%3N` взяты из
того же узла.

```bash
cat stand/spike/probe.sh | vm_put dv-1 /usr/local/bin/probe.sh
cat stand/spike/watch.sh | vm_put dv-1 /usr/local/bin/watch.sh
vm dv-1 "chmod +x /usr/local/bin/probe.sh /usr/local/bin/watch.sh"
vm dv-1 "nohup /usr/local/bin/probe.sh /mnt/spike /var/tmp/acked >/var/tmp/probe.log 2>&1 &"
vm dv-1 "nohup /usr/local/bin/watch.sh /var/tmp/acked >/var/tmp/watch.log 2>&1 &"
sleep 5 && vm dv-1 "cat /var/tmp/acked"
```

```text
964
```

Изоляция `dv-1` (правила `nft` из брифа; `date` до и после):

```text
21:34:40.231
21:34:40.258
table inet spike {
	chain in {
		type filter hook input priority filter; policy accept;
		tcp dport 7000 drop
		tcp sport 7000 drop
	}

	chain out {
		type filter hook output priority filter; policy accept;
		tcp dport 7000 drop
		tcp sport 7000 drop
	}
}
```

Через 20 с:

```bash
vm dv-1 "date +%T.%3N; cat /var/tmp/acked; cat /var/tmp/probe.log; pgrep -f probe.sh || echo 'probe exited'"
tb dv-1 "drbdadm status spike"
```

```text
21:35:00.405
988
sync: error syncing '/mnt/spike/log': Input/output error
write 989 failed
10539
10545
spike role:Primary
  disk:UpToDate quorum:no open:yes
  lima-dv-2 connection:Connecting
  lima-dv-3 connection:Connecting
```

Проверка `pgrep -f probe.sh` из брифа всегда находит процесс: под шаблон
попадает командная строка самой обёртки `sudo bash -c "… probe.sh …"` (PID
10539 и 10545). Писатель к этому моменту завершился: `ps -eo pid,stat,args |
grep '[p]robe.sh'` пуст, `watch.sh` показывает `probe=gone`. Выдержка из
`/var/tmp/watch.log` (повторяющиеся строки свёрнуты `uniq -f1`):

```text
21:34:39.645 acked=849 probe=alive:S
21:34:40.155 acked=969 probe=alive:S
21:34:40.675 acked=988 probe=alive:S
21:34:51.398 acked=988 probe=gone
```

`dmesg -T` на `dv-1` (потоки sender/receiver убраны):

```text
[Thu Nov 12 21:34:50 2026] drbd spike lima-dv-2: PingAck did not arrive in time.
[Thu Nov 12 21:34:50 2026] drbd spike lima-dv-2: conn( Connected -> NetworkFailure ) peer( Secondary -> Unknown )
[Thu Nov 12 21:34:50 2026] drbd spike/0 drbd1000 lima-dv-2: pdsk( UpToDate -> DUnknown ) repl( Established -> Off )
[Thu Nov 12 21:34:50 2026] drbd spike/0 drbd1000: gen-obligation: DISCHARGED(promoted) -> ARMED(peer-data-lost)
[Thu Nov 12 21:34:50 2026] drbd spike lima-dv-3: PingAck did not arrive in time.
[Thu Nov 12 21:34:50 2026] drbd spike lima-dv-3: conn( Connected -> NetworkFailure ) peer( Secondary -> Unknown )
[Thu Nov 12 21:34:50 2026] drbd spike/0 drbd1000: quorum( yes -> no )
[Thu Nov 12 21:34:50 2026] drbd spike/0 drbd1000 lima-dv-3: pdsk( UpToDate -> DUnknown ) repl( Established -> Off )
[Thu Nov 12 21:34:50 2026] drbd spike/0 drbd1000: gen-obligation: ARMED(peer-data-lost) -> PARKED(peer-data-lost)
[Thu Nov 12 21:34:50 2026] drbd spike lima-dv-3: Connection closed
[Thu Nov 12 21:34:50 2026] drbd spike lima-dv-3: conn( NetworkFailure -> Unconnected ) [disconnected]
[Thu Nov 12 21:34:50 2026] drbd spike lima-dv-3: conn( Unconnected -> Connecting ) [connecting]
[Thu Nov 12 21:34:51 2026] drbd spike lima-dv-2: Connection closed
[Thu Nov 12 21:34:51 2026] drbd spike lima-dv-2: conn( NetworkFailure -> Unconnected ) [disconnected]
[Thu Nov 12 21:34:51 2026] Aborting journal on device drbd1000-8.
[Thu Nov 12 21:34:51 2026] drbd spike lima-dv-2: conn( Unconnected -> Connecting ) [connecting]
[Thu Nov 12 21:34:51 2026] Buffer I/O error on dev drbd1000, logical block 65536, lost sync page write
[Thu Nov 12 21:34:51 2026] JBD2: I/O error when updating journal superblock for drbd1000-8.
```

Таймауты сети ресурса по умолчанию (`drbdsetup show spike --show-defaults`):

```text
            timeout         	60; # 1/10 seconds, default
            connect-int     	10; # seconds, default
            ping-int        	10; # seconds, default
            ko-count        	7; # default
            ping-timeout    	5; # 1/10 seconds, default
```

`dmesg -T` даёт время с точностью до секунды; соседние события в `watch.log`
точнее.

| Интервал | Значение |
|---|---|
| Изоляция → последнее подтверждение (`acked=988`) | < 0.5 с (21:34:40.24 → 21:34:40.675) |
| Изоляция → `PingAck did not arrive in time`, `quorum( yes -> no )` | ~10 с (21:34:40 → 21:34:50); по величине совпадает с `ping-int 10` + `ping-timeout 0.5` [проверить: механизм] |
| Изоляция → писатель завершился с `EIO` | 10–11 с (`probe=gone` в 21:34:51.398) |
| Всё это время писатель | номер не растёт; `watch.sh` показывал `S` — это состояние родительского `bash`, состояние дочернего `sync` в этом прогоне не снималось (в шаге 7 и опыте E1 оно снято отдельно) |

Журнал ext4 прерван (`Aborting journal`). Вывод `mount` после ошибки в этом
прогоне не сохранён; в опыте E1 `findmnt` после той же ошибки показал
`rw,relatime`, а новые записи получали `Read-only file system`.

`drbdsetup status spike --json` в этот момент сохранён: на `dv-2` —
`stand/spike/samples/peer-disconnected.json` (одно соединение `Connecting`,
`quorum: true`), на `dv-1` — `stand/spike/samples/quorum-lost.json`
(`quorum: false`, оба соединения `Connecting`, `role: Primary`). Вид с `dv-2`:

```text
spike role:Secondary
  disk:UpToDate open:no
  lima-dv-1 connection:Connecting
  lima-dv-3 role:Secondary
    peer-disk:UpToDate
```

В `dmesg` `dv-3` после пропажи `dv-1`:
`No reconciliation resync even though 'lima-dv-1' disappeared. (o=0)`.

Монтирование на `dv-2` и сверка подтверждённых записей:

```bash
vm dv-1 "cat /var/tmp/acked"
vm dv-2 "date +%T.%3N; mount /dev/drbd1000 /mnt/spike && tail -1 /mnt/spike/log; date +%T.%3N"; echo "mount rc=$?"
vm dv-2 "wc -l < /mnt/spike/log; sort -n /mnt/spike/log | uniq -d | head; awk 'NR!=\$1{print \"gap at line \" NR\": \"\$1; exit}' /mnt/spike/log; dmesg | tail -3"
tb dv-2 "drbdadm status spike | head -2"
```

```text
988
21:35:35.835
989
21:35:35.881
mount rc=0
989
[ 1545.902172] drbd spike/0 drbd1000: gen-obligation: MINTING(promoted) -> DISCHARGED(promoted)
[ 1545.921347] EXT4-fs (drbd1000): recovery complete
[ 1545.923492] EXT4-fs (drbd1000): mounted filesystem 865ae72a-83a0-4dff-ab2e-9486c4d54258 r/w with ordered data mode. Quota mode: none.
spike role:Primary
  disk:UpToDate open:yes
```

Подтверждено 988, в журнале на томе на `dv-2` последнее число 989; строк 989,
повторов и пропусков нет. Подтверждённые записи на месте. Запись 989 дошла до
пиров, но `fsync` на `dv-1` вернул ошибку, поэтому номер не подтверждён.

Размонтирование на изолированном узле:

```bash
vm dv-1 "umount /mnt/spike"; echo "rc=$?"
vm dv-1 "mount | grep spike || echo not-mounted"
tb dv-1 "drbdadm status spike"
```

```text
rc=0
not-mounted
spike role:Secondary
  disk:UpToDate quorum:no open:no
  lima-dv-2 connection:Connecting
  lima-dv-3 connection:Connecting
```

`umount` прошёл сразу, `dv-1` вернулся в `Secondary` без кворума.

Возврат сети и наблюдение за `dv-1` (опрос `drbdsetup status spike` в цикле):

```text
21:35:45.011
21:35:45.041
21:35:45 spike role:Secondary disk:UpToDate quorum:no open:no lima-dv-2 connection:Connecting lima-dv-3 connection:Connecting
21:35:47 spike role:Secondary disk:UpToDate quorum:no open:no lima-dv-2 connection:Connecting lima-dv-3 connection:Connecting
21:35:49 spike role:Secondary disk:UpToDate quorum:no open:no lima-dv-2 connection:Connecting lima-dv-3 connection:Connecting
21:35:50 spike role:Secondary disk:Inconsistent open:no lima-dv-2 connection:Connecting lima-dv-3 role:Secondary replication:SyncTarget peer-disk:UpToDate
21:35:53 spike role:Secondary disk:UpToDate open:no lima-dv-2 role:Primary peer-disk:UpToDate lima-dv-3 role:Secondary peer-disk:UpToDate
```

`dmesg -T` на `dv-1`:

```text
[Thu Nov 12 21:35:50 2026] drbd spike/0 drbd1000 lima-dv-3: self 02BE5916891BC44C:0000000000000000:0000000000000000:0000000000000000 bits:1 flags:2520
[Thu Nov 12 21:35:50 2026] drbd spike/0 drbd1000 lima-dv-3: peer 8AF9E2CE90BD4542:02BE5916891BC44C:02BE5916891BC44C:0000000000000000 bits:7 flags:2100
[Thu Nov 12 21:35:50 2026] drbd spike/0 drbd1000 lima-dv-3: uuid_compare()=target-use-bitmap by rule=bitmap-peer
[Thu Nov 12 21:35:50 2026] drbd spike/0 drbd1000: disk( UpToDate -> Outdated ) quorum( no -> yes ) [connected]
[Thu Nov 12 21:35:50 2026] drbd spike/0 drbd1000: disk( Outdated -> Inconsistent ) [receive-bitmap]
[Thu Nov 12 21:35:50 2026] drbd spike/0 drbd1000 lima-dv-3: repl( WFBitMapT -> SyncTarget ) [receive-bitmap]
[Thu Nov 12 21:35:50 2026] drbd spike/0 drbd1000 lima-dv-3: Began resync as SyncTarget (will sync 32 KB [8 bits set]).
[Thu Nov 12 21:35:50 2026] drbd spike/0 drbd1000 lima-dv-3: Resync done (total 1 sec; paused 0 sec; 32 K/sec)
[Thu Nov 12 21:35:51 2026] drbd spike/0 drbd1000 lima-dv-2: uuid_compare()=target-use-bitmap by rule=bitmap-peer
[Thu Nov 12 21:35:51 2026] drbd spike lima-dv-2: conn( Connecting -> Connected ) peer( Unknown -> Primary ) [connected]
[Thu Nov 12 21:35:51 2026] drbd spike/0 drbd1000 lima-dv-2: Began resync as SyncTarget (will sync 0 KB [0 bits set]).
[Thu Nov 12 21:35:51 2026] drbd spike/0 drbd1000: disk( Inconsistent -> UpToDate ) [resync-finished]
```

| Интервал | Значение |
|---|---|
| Возврат сети → соединение с `dv-3` | ~5 с (21:35:45.0 → 21:35:50), повтор соединения раз в `connect-int 10` |
| Возврат сети → `dv-1` `UpToDate`, оба соединения `Connected` | ~6 с (до 21:35:51) |
| Объём досылки | 32 КиБ по битовой карте (8 бит), полной синхронизации нет |

`StandAlone` нет, `grep -i split` в `dmesg` `dv-1` и `dv-2` пуст. Образец
`stand/spike/samples/peer-primary.json` снят на `dv-1` после возврата:
`dv-1` `Secondary`, пир `dv-2` `Primary`. Досылка длилась меньше секунды, и
состояние `SyncTarget` в `--json` поймать не удалось: опрос раз в ~1.5 с попал
в момент после её окончания.

```bash
vm dv-2 "umount /mnt/spike"
```

```text
umount-dv2 rc=0
```

### Шаг 7. Та же изоляция с `on-no-quorum suspend-io`

Смена политики:

```bash
for n in dv-1 dv-2 dv-3; do
  vm "$n" "sed -i 's/on-no-quorum io-error/on-no-quorum suspend-io/' /var/lib/rancher/ambar/res/spike.res"
  tb "$n" "drbdadm adjust spike"; echo "$n adjust rc=$?"
done
tb dv-1 "drbdsetup show spike | grep on-no-quorum"
```

```text
dv-1 adjust rc=0
dv-2 adjust rc=0
dv-3 adjust rc=0
command terminated with exit code 1
```

Ожидаемой строки `on-no-quorum suspend-io;` нет: `drbdsetup show` печатает
только значения, отличные от значений по умолчанию, а `suspend-io` в 9.3.4 —
значение по умолчанию. Проверка с `--show-defaults`:

```bash
tb dv-1 "drbdsetup show spike --show-defaults | grep -E quorum"
```

```text
        quorum          	majority;
        on-no-quorum    	suspend-io; # default
        quorum-minimum-redundancy	off; # default
```

`drbdadm -d adjust spike` после смены пуст: изменение уже применено ядром.
Следствие для генератора `.res`: без явной строки `on-no-quorum` ресурс
работает с `suspend-io`.

Повтор шага 6. Перед запуском писателя удалены `/var/tmp/acked`,
`/var/tmp/probe.log`, `/var/tmp/watch.log`. Журнал на томе `/mnt/spike/log` не
удалялся, новый прогон дописывает числа с 1 после 989 строк первого прогона.

```text
mount rc=0
spike role:Primary
  disk:UpToDate open:yes
1015
21:37:39.175
21:37:39.205
```

Через 20 с после изоляции:

```bash
vm dv-1 "date +%T.%3N; cat /var/tmp/acked; cat /var/tmp/probe.log; ps -eo pid,stat,wchan:32,args | grep '[p]robe.sh' || echo 'probe exited'"
vm dv-1 "pgrep -f probe.sh && cat /proc/\$(pgrep -f probe.sh | head -1)/status | grep State"
vm dv-1 "for p in \$(pgrep -x sync) \$(pgrep -f /usr/local/bin/probe.sh); do echo pid \$p; grep State /proc/\$p/status; cat /proc/\$p/stack | head -8; done"
tb dv-1 "drbdadm status spike"
```

```text
21:37:59.336
1046
  14778 S    do_wait                          bash /usr/local/bin/probe.sh /mnt/spike /var/tmp/acked
14778
16328
16334
State:	S (sleeping)
pid 15951
State:	D (disk sleep)
[<0>] jbd2_log_wait_commit+0x15c/0x218 [jbd2]
[<0>] jbd2_complete_transaction+0x80/0x1a0 [jbd2]
[<0>] ext4_fc_commit+0x1f4/0x718 [ext4]
[<0>] ext4_sync_file+0xdc/0x2d0 [ext4]
[<0>] vfs_fsync_range+0x3c/0x90
[<0>] do_fsync+0x44/0xa8
[<0>] __arm64_sys_fsync+0x20/0x40
[<0>] invoke_syscall+0x60/0xe0
pid 14778
State:	S (sleeping)
[<0>] do_wait+0x68/0x108
...
spike role:Primary suspended:quorum
  disk:UpToDate quorum:no open:yes blocked:upper
  lima-dv-2 connection:Connecting
  lima-dv-3 connection:Connecting
```

Писатель жив: `bash` ждёт дочерний `sync` (PID 15951), который висит в
`fsync` в состоянии `D`. Команда из брифа `cat /proc/<pgrep -f>/status`
показывает состояние `bash` (`S`), а не зависшего `sync`. `dmesg -T`:

```text
[Thu Nov 12 21:37:49 2026] drbd spike lima-dv-2: PingAck did not arrive in time.
[Thu Nov 12 21:37:49 2026] drbd spike lima-dv-2: conn( Connected -> NetworkFailure ) peer( Secondary -> Unknown )
[Thu Nov 12 21:37:49 2026] drbd spike lima-dv-3: PingAck did not arrive in time.
[Thu Nov 12 21:37:49 2026] drbd spike: susp-io( no -> quorum )
[Thu Nov 12 21:37:49 2026] drbd spike/0 drbd1000: quorum( yes -> no )
```

Ошибок ext4 и `EIO` в этот момент нет. `drbdsetup status spike --json` на
`dv-1` сохранён в `stand/spike/samples/quorum-suspended.json`:
`"suspended": true`, `"suspended-quorum": true`, `"quorum": false`.

`watch.log`:

```text
21:37:39.038 acked=1015 probe=alive:S
21:37:39.550 acked=1046 probe=alive:S
21:38:09.179 acked=1046 probe=alive:S
```

Монтирование на `dv-2` и сверка:

```text
1046
21:38:09.409
1046
21:38:09.427
mount rc=0
2035
```

```bash
vm dv-2 "sed -n 989,990p /mnt/spike/log; tail -n +990 /mnt/spike/log | awk 'NR!=\$1{print \"gap at \" NR; bad=1} END{print \"run2 lines=\" NR \" bad=\" bad+0}'"
```

```text
989
1
run2 lines=1046 bad=0
```

Подтверждено 1046, в журнале на томе последнее число 1046; второй прогон
занимает строки 990–2035 без пропусков. Подтверждённые записи на месте.

Размонтирование на изолированном узле (команды из таблицы брифа):

```text
21:38:23.804
umount: /mnt/spike: target is busy.
rc=32
21:38:23.906
  15951 D    jbd2_log_wait_commit             sync /mnt/spike/log
21:38:24.005
rc=0
21:38:24.098
not-in-mount-table
  14778 S    do_wait                          bash /usr/local/bin/probe.sh /mnt/spike /var/tmp/acked
  15951 D    jbd2_log_wait_commit             sync /mnt/spike/log
spike role:Primary suspended:quorum
  disk:UpToDate quorum:no open:yes blocked:upper
```

`timeout 20 umount` не зависает, а сразу возвращает `target is busy` (код 32):
файл держит процесс в `D`. `umount -l` возвращает 0 и убирает точку
монтирования из таблицы, но устройство остаётся открытым (`open:yes`), узел
остаётся `Primary`.

Попытка освободить устройство:

```bash
vm dv-1 "kill -9 \$(pgrep -x sync); sleep 5; ps -eo pid,stat,wchan:32,args | grep -E '[s]ync |[p]robe' || echo none"
tb dv-1 "timeout 15 drbdadm secondary spike"; echo secondary rc=$?
```

```text
  15951 D    jbd2_log_wait_commit             sync /mnt/spike/log
spike: State change failed: (-12) Device is held open by someone
additional info from kernel:
/dev/drbd1000 open_cnt:1, writable:1; list of openers follows
drbd1000 opened by mount (pid 14689) at 2026-11-12 13:37:33.007
Command 'drbdsetup secondary spike' terminated with exit code 11
secondary rc=11
```

`SIGKILL` не снимает процесс в `D`; `drbdadm secondary` отвергнут, пока
устройство открыто. Процесс `bash` писателя был снят раньше: `pkill -9 -f
/usr/local/bin/probe.sh` внутри `vm` убил и саму обёртку `bash -c`, в
командной строке которой есть тот же шаблон, поэтому вывод той команды пуст.

Возврат сети (21:38:56.1) и опрос `drbdsetup status spike` на `dv-1`:

```text
21:38:56.098
21:38:56.113
21:38:56 spike role:Primary suspended:quorum disk:UpToDate quorum:no open:yes blocked:upper lima-dv-2 connection:Connecting lima-dv-3 connection:Connecting
21:38:58 spike role:Secondary disk:UpToDate open:no lima-dv-2 role:Primary peer-disk:UpToDate lima-dv-3 connection:StandAlone
...
21:39:24 spike role:Secondary disk:UpToDate open:no lima-dv-2 role:Primary peer-disk:UpToDate lima-dv-3 connection:StandAlone
```

`dmesg -T` на `dv-1`:

```text
[Thu Nov 12 21:38:57 2026] drbd spike/0 drbd1000 lima-dv-3: uuid_compare()=target-use-bitmap by rule=bitmap-peer
[Thu Nov 12 21:38:57 2026] drbd spike/0 drbd1000 lima-dv-3: I shall become SyncTarget, but I am primary!
[Thu Nov 12 21:38:57 2026] drbd spike: Aborting cluster-wide state change 4171366112 (20ms) rv = -27
[Thu Nov 12 21:38:57 2026] drbd spike lima-dv-3: conn( Connecting -> Disconnecting ) [connect-failed]
[Thu Nov 12 21:38:57 2026] drbd spike/0 drbd1000: disk( UpToDate -> Outdated ) [connect-failed]
[Thu Nov 12 21:38:57 2026] Aborting journal on device drbd1000-8.
[Thu Nov 12 21:38:57 2026] Buffer I/O error on dev drbd1000, logical block 65536, lost sync page write
[Thu Nov 12 21:38:57 2026] JBD2: I/O error when updating journal superblock for drbd1000-8.
[Thu Nov 12 21:38:57 2026] EXT4-fs (drbd1000): unmounting filesystem 865ae72a-83a0-4dff-ab2e-9486c4d54258.
[Thu Nov 12 21:38:57 2026] Buffer I/O error on dev drbd1000, logical block 0, lost sync page write
[Thu Nov 12 21:38:57 2026] EXT4-fs (drbd1000): I/O error while writing superblock
[Thu Nov 12 21:38:57 2026] drbd spike: role( Primary -> Secondary ) [sync:15951 auto-demote]
[Thu Nov 12 21:38:57 2026] drbd spike lima-dv-3: conn( Disconnecting -> StandAlone ) [disconnected]
[Thu Nov 12 21:38:57 2026] drbd spike/0 drbd1000 lima-dv-2: uuid_compare()=target-use-bitmap by rule=bitmap-peer
[Thu Nov 12 21:38:57 2026] drbd spike: susp-io( quorum -> no ) [connected]
[Thu Nov 12 21:38:57 2026] drbd spike lima-dv-2: conn( Connecting -> Connected ) peer( Unknown -> Primary ) [connected]
[Thu Nov 12 21:38:57 2026] drbd spike/0 drbd1000: quorum( no -> yes ) [connected]
[Thu Nov 12 21:38:57 2026] drbd spike/0 drbd1000 lima-dv-2: Began resync as SyncTarget (will sync 32 KB [8 bits set]).
[Thu Nov 12 21:38:57 2026] drbd spike/0 drbd1000 lima-dv-2: Resync done (total 1 sec; paused 0 sec; 32 K/sec)
[Thu Nov 12 21:38:57 2026] drbd spike/0 drbd1000: disk( Inconsistent -> UpToDate ) [resync-finished]
```

`dmesg -T` на `dv-3`:

```text
[Thu Nov 12 21:38:57 2026] drbd spike/0 drbd1000 lima-dv-1: uuid_compare()=source-use-bitmap by rule=bitmap-self
[Thu Nov 12 21:38:57 2026] drbd spike lima-dv-1: Aborting remote state change 4171366112
[Thu Nov 12 21:38:57 2026] drbd spike lima-dv-1: conn( Connecting -> Disconnecting ) [receive-disconnect]
[Thu Nov 12 21:38:57 2026] drbd spike lima-dv-1: conn( Disconnecting -> StandAlone ) [disconnected]
```

Порядок событий при возврате сети: первым соединяется `dv-3`; `dv-1` всё ещё
`Primary` с приостановленным вводом-выводом и должен стать `SyncTarget`, что
запрещено для `Primary`. Соединение `dv-1`↔`dv-3` переходит в `StandAlone` на
обеих сторонах, диск `dv-1` помечается `Outdated`, приостановленные запросы
завершаются с `EIO`, ext4 на `dv-1` отключается, `sync` выходит, устройство
закрывается и `dv-1` переходит в `Secondary`. Затем `dv-1` соединяется с
`dv-2` и досылает 32 КиБ. Сообщения `Split-Brain detected` нет (`grep -i
split` пуст на трёх узлах), но `StandAlone` сам не снимается: через 28 с
соединение `dv-1`↔`dv-3` всё ещё `StandAlone`.

Приостановленные записи `dv-1` не попали на `dv-2`: после размонтирования на
`dv-2` журнал цел, `fsck` чистый.

```bash
vm dv-2 "tail -1 /mnt/spike/log; wc -l < /mnt/spike/log; umount /mnt/spike; echo umount rc=\$?; fsck.ext4 -n -f /dev/drbd1000; echo fsck rc=\$?"
```

```text
1046
2035
umount rc=0
e2fsck 1.47.2 (1-Jan-2025)
Pass 1: Checking inodes, blocks, and sizes
Pass 2: Checking directory structure
Pass 3: Checking directory connectivity
Pass 4: Checking reference counts
Pass 5: Checking group summary information
/dev/drbd1000: 13/65536 files (7.7% non-contiguous), 8924/262119 blocks
fsck rc=0
```

Снятие `StandAlone`: `drbdadm adjust spike` нужен на обеих сторонах
соединения. После `adjust` на `dv-1` (21:39:44.9) его сторона перешла в
`Connecting`, сторона `dv-3` осталась `StandAlone`. После `adjust` на `dv-3`
(21:39:55.9):

```text
[Thu Nov 12 21:39:56 2026] drbd spike lima-dv-1: conn( StandAlone -> Unconnected ) [connect]
[Thu Nov 12 21:39:56 2026] drbd spike/0 drbd1000 lima-dv-1: uuid_compare()=no-sync by rule=reconnected
[Thu Nov 12 21:39:56 2026] drbd spike lima-dv-1: conn( Connecting -> Connected ) peer( Unknown -> Secondary ) [remote]
```

Все три стороны `Connected`, `UpToDate`, досылки нет.

| Наблюдение | `io-error`, том снят до возврата сети (шаг 6) | `suspend-io` (шаг 7) | `io-error`, том не снят до возврата сети (E1) |
|---|---|---|---|
| Изоляция → потеря кворума | ~10 с | ~10 с | ~10 с |
| Писатель после потери кворума | `fsync` → `EIO` через 10–11 с после изоляции, процесс завершился | `sync` висит в `D` до возврата сети (77 с в опыте), `SIGKILL` не действует | `fsync` → `EIO` через 10–11 с, процесс завершился |
| `umount` на изолированном узле | `rc=0`, узел перешёл в `Secondary` | `target is busy`, `rc=32`; `umount -l` `rc=0`, но устройство открыто, узел `Primary` | не выполнялся до возврата сети; после — `rc=0`, узел перешёл в `Secondary` |
| Подтверждённые записи после монтирования на `dv-2` | ACKED 988, на томе 989 | ACKED 1046, на томе 1046 | ACKED 1402, на томе 1402 |
| Роль `dv-1` в момент возврата сети | `Secondary` | `Primary` | `Primary` |
| Возврат сети → все соединения `Connected` | ~6 с, без вмешательства | `dv-1`↔`dv-2` за ~1 с (после того как ввод-вывод завершился с `EIO` и узел ушёл в `Secondary`); `dv-1`↔`dv-3` в `StandAlone` до `drbdadm adjust` на обоих узлах | обе связи `dv-1` в `StandAlone`; размонтирование их не сняло; сняли `drbdadm adjust` на `dv-1`, `dv-2`, `dv-3` |

Возврат `io-error`:

```text
dv-1 adjust rc=0
        on-no-quorum    	io-error;
dv-2 adjust rc=0
        on-no-quorum    	io-error;
dv-3 adjust rc=0
        on-no-quorum    	io-error;
```

### Шаг 8. Сторона без диска, ресурс `spike2`

```bash
stand/spike/res.sh spike2 1001 7001 dv-3 io-error
```

Скрипт отработал без правок. Блок узла без диска в `spike2.res`:

```text
    on lima-dv-3 {
        node-id 2;
        address ipv4 192.168.104.4:7001;
        volume 0 { device minor 1001; disk none; }
    }
```

Вывод (хвост):

```text
initializing bitmap (32 KB) to all zero
initializing activity log
Writing meta data...
New drbd meta data block successfully created.
...
spike2 role:Secondary
  disk:Inconsistent open:no
  lima-dv-2 role:Secondary
    peer-disk:Inconsistent
  lima-dv-3 role:Secondary
    peer-disk:Diskless peer-client:yes
```

`create-md` для `spike2` выделил битовую карту 32 КиБ против 64 КиБ у
`spike`: место под битовую карту отведено только для одного пира с диском;
`size` устройства `1048508` КиБ против `1048476` у `spike`. Перевод стороны без
диска в сторону с копией на этих метаданных не проверялся [проверить: нужен ли
`create-md` с `--max-peers` при смене `replicas: 2` → `3`].

Пропуск синхронизации:

```bash
tb dv-1 "drbdadm new-current-uuid --clear-bitmap spike2/0"; echo rc=$?
sleep 3; tb dv-3 "drbdadm status spike2"; tb dv-1 "drbdadm status spike2"
```

```text
rc=0
spike2 role:Secondary
  disk:Diskless client:yes open:no
  lima-dv-1 role:Secondary
    peer-disk:UpToDate
  lima-dv-2 role:Secondary
    peer-disk:UpToDate

spike2 role:Secondary
  disk:UpToDate open:no
  lima-dv-2 role:Secondary
    peer-disk:UpToDate
  lima-dv-3 role:Secondary
    peer-disk:Diskless peer-client:yes
```

Образец `stand/spike/samples/diskless-node.json` снят на `dv-3`:
`"disk-state": "Diskless"`, `"client": true`, `"quorum": true`.

Открытие на узле без диска и запись:

```text
mkfs rc=0
mount rc=0
1414
spike2 role:Primary
  disk:Diskless client:yes open:yes
```

Штатная остановка `dv-2` (`limactl stop`, узел гасится 31 с: 21:41:49 → 21:42:20):

```bash
vm dv-3 "date +%T.%3N"; limactl stop dv-2; vm dv-3 "date +%T.%3N"
sleep 15
vm dv-3 "a=\$(cat /var/tmp/acked2); sleep 5; b=\$(cat /var/tmp/acked2); echo \"\$a -> \$b\"; pgrep -x probe.sh >/dev/null && echo 'probe alive'"
tb dv-3 "drbdadm status spike2"
tb dv-1 "drbdadm status spike2"
```

```text
21:41:49.547
time="2026-11-12T21:41:49+08:00" level=info msg="Sending SIGINT to hostagent process 95046"
...
time="2026-11-12T21:42:20+08:00" level=info msg="The instance dv-2 has shut down"
stop rc=0
21:42:20.303
23065 -> 25566
spike2 role:Primary
  disk:Diskless client:yes open:yes
  lima-dv-1 role:Secondary
    peer-disk:UpToDate
  lima-dv-2 connection:Connecting

spike2 role:Secondary
  disk:UpToDate open:no
  lima-dv-2 connection:Connecting
  lima-dv-3 role:Primary
    peer-disk:Diskless peer-client:yes
```

Номер растёт (23065 → 25566 за 5 с). Строки `probe alive` нет из-за самой
проверки: процесс писателя называется `bash` (`ps -eo pid,comm,args` →
`5324 bash bash /usr/local/bin/probe.sh /mnt/spike2 /var/tmp/acked2`), и
`pgrep -x probe.sh` его не находит. `watch.sh` на `dv-3` вокруг остановки:

```text
21:41:50.570 acked=5232 probe=alive:S
21:41:51.080 acked=5268 probe=alive:S
21:41:51.590 acked=5268 probe=alive:S
...
21:41:55.686 acked=5268 probe=alive:S
21:41:56.193 acked=5381 probe=alive:S
21:41:56.701 acked=5642 probe=alive:S
```

`dmesg -T` на `dv-3`; строк `quorum( yes -> no )` для `spike2` — 0:

```text
[Thu Nov 12 21:41:55 2026] drbd spike2 lima-dv-2: PingAck did not arrive in time.
[Thu Nov 12 21:41:55 2026] drbd spike2 lima-dv-2: conn( Connected -> NetworkFailure ) peer( Secondary -> Unknown )
[Thu Nov 12 21:41:55 2026] drbd spike2/0 drbd1001 lima-dv-2: pdsk( UpToDate -> DUnknown ) repl( Established -> Off )
[Thu Nov 12 21:41:55 2026] drbd spike2/0 drbd1001: sending new current UUID: 346F69B621D07BC5
[Thu Nov 12 21:41:55 2026] drbd spike2/0 drbd1001: rotated data generation confirmed durable in a quorate partition (gen 346F69B621D07BC5)
```

| Интервал | Значение |
|---|---|
| Пауза записи при штатной остановке `dv-2` | 5.1 с между выборками с ростом (21:41:51.080 → 21:41:56.193); с учётом шага выборки 0.5 с — от 4.6 до 5.6 с. Пауза кончается вместе с `PingAck did not arrive in time` об `dv-2` в 21:41:55 [проверить: какой параметр задаёт длину паузы] |
| Кворум у `dv-3` (без диска) + `dv-1` (с диском) | сохранён, `"quorum": true` |

Образцы: `stand/spike/samples/diskless-peer-down.json` (на `dv-3`, один пир
`Connecting`, `quorum: true`), `stand/spike/samples/peer-disconnected-diskful.json`
(на `dv-1`).

Возврат `dv-2`:

```bash
limactl start --tty=false dv-2
kc wait --for=condition=Ready node --all --timeout=180s
kc -n ambar-spike rollout status ds/toolbox --timeout=120s
tb dv-2 "/opt/ambar/load-module.sh"
vm dv-2 "mkdir -p /dev/ambar; for r in spike spike2; do f=/var/lib/rancher/ambar/volumes/\$r.img; ln -sfn \$(losetup --find --show --direct-io=on \$f) /dev/ambar/\$r; done"
tb dv-2 "drbdadm up spike; drbdadm up spike2"
```

```text
start rc=0
node/lima-dv-1 condition met
node/lima-dv-2 condition met
node/lima-dv-3 condition met
daemon set "toolbox" successfully rolled out
toolbox-g9ckc   1/1     Running   1 (11s ago)   14m   192.168.104.3   lima-dv-2   <none>           <none>
no-drbd-module
loaded drbd 9.3.4 for kernel 6.12.116+deb13-arm64
load rc=0
9.3.4
disabled
NAME       BACK-FILE                                  DIO
/dev/loop1 /var/lib/rancher/ambar/volumes/spike2.img   1
/dev/loop0 /var/lib/rancher/ambar/volumes/spike.img    1
```

Адрес `dv-2` после перезапуска прежний (192.168.104.3). `drbdadm up` в
21:43:38.9. Досылка `spike2` шла под нагрузкой писателя на `dv-3`; состояние
`drbdadm status` в 21:43:58:

```text
spike2 role:Secondary
  disk:Inconsistent open:no
  lima-dv-1 role:Secondary
    replication:SyncTarget peer-disk:UpToDate done:99.98
  lima-dv-3 role:Primary
    peer-disk:Diskless peer-client:yes resync-suspended:dependency
```

`dmesg -T` на `dv-2` для `spike2`:

```text
[Thu Nov 12 21:43:39 2026] drbd spike2/0 drbd1001 lima-dv-1: Began resync as SyncTarget (will sync 16668 KB [4167 bits set]).
[Thu Nov 12 21:43:40 2026] drbd spike2/0 drbd1001 lima-dv-1: Becoming WFBitMapT because primary is diskless
[Thu Nov 12 21:43:40 2026] drbd spike2: State change failed: Can not start OV/resync since it is already active (-8)
[Thu Nov 12 21:43:40 2026] drbd spike2/0 drbd1001 lima-dv-1: ...postponing this until current resync finished
[Thu Nov 12 21:44:08 2026] drbd spike2/0 drbd1001 lima-dv-1: Resync done (total 28 sec; paused 0 sec; 1368 K/sec)
[Thu Nov 12 21:44:08 2026] drbd spike2/0 drbd1001 lima-dv-1: repl( Established -> WFBitMapT ) [resync-again]
[Thu Nov 12 21:44:08 2026] drbd spike2/0 drbd1001 lima-dv-1: Began resync as SyncTarget (will sync 452 KB [113 bits set]).
[Thu Nov 12 21:44:09 2026] drbd spike2/0 drbd1001 lima-dv-1: Resync done (total 1 sec; paused 0 sec; 452 K/sec)
[Thu Nov 12 21:44:09 2026] drbd spike2/0 drbd1001: disk( Inconsistent -> UpToDate ) [resync-finished]
```

Для `spike` (запись за время отсутствия не шла):

```text
[Thu Nov 12 21:43:39 2026] drbd spike/0 drbd1000: disk( Attaching -> UpToDate ) [attach]
[Thu Nov 12 21:43:39 2026] drbd spike/0 drbd1000 lima-dv-1: uuid_compare()=no-sync by rule=lost-quorum
[Thu Nov 12 21:43:39 2026] drbd spike/0 drbd1000 lima-dv-3: uuid_compare()=no-sync by rule=lost-quorum
```

| Интервал | Значение |
|---|---|
| `drbdadm up` → `spike2` `UpToDate` на `dv-2` | ~30 с (21:43:38.9 → 21:44:09), 16668 КиБ + 452 КиБ по битовой карте при работающем писателе |
| Вид досылки | по битовой карте (`target-use-bitmap`), не полная: 16668 КиБ из 1048508 КиБ |
| `spike` | без досылки, `UpToDate` сразу после `attach` |

Образец `stand/spike/samples/resync-target.json` снят на `dv-2` в ходе этой
досылки: `"disk-state": "Inconsistent"`, `"replication-state": "SyncTarget"`,
`"has-sync-details": true` и поля `rs-total`, `rs-dt-start-ms`,
`percent-resync-done`.

Остановка писателя и сверка на `dv-3`:

```text
acked2=60626
60627
60627
umount rc=0
spike2 role:Secondary
  disk:Diskless client:yes open:no
```

Писатель снят `SIGTERM` на том же узле, переключения писателя на другой узел
не было: в журнале 60627 строк без пропусков, подтверждено 60626.

### Шаг 9. Замер `fsync` (стенд, не R5C)

`fsync-bench.sh` из брифа на `dv-1`, запущен два раза подряд:

```text
/var/tmp/local: 919 us per fsync
/mnt/spike: 3918 us per fsync
/var/tmp/local: 1323 us per fsync
/mnt/spike: 2411 us per fsync
umount rc=0
```

| Каталог | Прогон 1 | Прогон 2 |
|---|---|---|
| `/var/tmp/local` (локальный диск узла) | 919 мкс | 1323 мкс |
| `/mnt/spike` (DRBD, `protocol C`, три копии, loop с прямым вводом-выводом) | 3918 мкс | 2411 мкс |

Время включает запуск процесса `dd` на каждую запись. Стенд, не R5C; разброс
между прогонами до 1.6 раза.

### Источник значений таблицы «Окружение»

```bash
limactl --version; limactl list --format '{{.Name}} {{.VMType}} {{.CPUs}} {{.Memory}}'
for n in dv-1 dv-2 dv-3; do vm $n "uname -n; k3s --version | head -1; nproc; free -m | head -2; ip -4 -o addr show dev eth0"; done
kc get nodes --no-headers
```

```text
limactl version 2.3.0
dv-1 vz 2 3221225472
dv-2 vz 2 3221225472
dv-3 vz 2 3221225472
== dv-1
lima-dv-1
k3s version v1.37.2+k3s1 (f6973cb8)
2
               total        used        free      shared  buff/cache   available
Mem:            2911        1185         268           1        1618        1726
2: eth0    inet 192.168.104.1/24 metric 200 brd 192.168.104.255 scope global dynamic eth0\       valid_lft 2350sec preferred_lft 2350sec
== dv-2
lima-dv-2
k3s version v1.37.2+k3s1 (f6973cb8)
2
               total        used        free      shared  buff/cache   available
Mem:            2911         763        1838           0         442        2148
2: eth0    inet 192.168.104.3/24 metric 200 brd 192.168.104.255 scope global dynamic eth0\       valid_lft 3544sec preferred_lft 3544sec
== dv-3
lima-dv-3
k3s version v1.37.2+k3s1 (f6973cb8)
2
               total        used        free      shared  buff/cache   available
Mem:            2911         800        1500           0         753        2111
2: eth0    inet 192.168.104.4/24 metric 200 brd 192.168.104.255 scope global dynamic eth0\       valid_lft 2761sec preferred_lft 2761sec
lima-dv-1   Ready   control-plane,etcd   40m   v1.37.2+k3s1
lima-dv-2   Ready   control-plane,etcd   40m   v1.37.2+k3s1
lima-dv-3   Ready   control-plane,etcd   39m   v1.37.2+k3s1
```

### Опыт E1. `io-error`, изолированный узел остаётся `Primary` при возврате сети

Цель: отделить влияние политики `on-no-quorum` от роли узла в момент возврата
сети. Перед опытом стенд очищен `stand/spike/clean.sh`, ресурс `spike` создан
заново. Один прогон.

```bash
stand/spike/res.sh spike 1000 7000 - io-error
vm dv-1 "grep on-no-quorum /var/lib/rancher/ambar/res/spike.res"
tb dv-1 "drbdadm new-current-uuid --clear-bitmap spike/0"; sleep 3
tb dv-1 "drbdsetup show spike | grep on-no-quorum; drbdadm status spike"
tb dv-1 "mkfs.ext4 -q /dev/drbd1000"
vm dv-1 "mkdir -p /mnt/spike && mount /dev/drbd1000 /mnt/spike; findmnt /mnt/spike"
# probe.sh и watch.sh запущены на dv-1 так же, как в шаге 6
```

```text
    options { quorum majority; on-no-quorum io-error; auto-promote yes; }
        on-no-quorum    	io-error;
spike role:Secondary
  disk:UpToDate open:no
  lima-dv-2 role:Secondary
    peer-disk:UpToDate
  lima-dv-3 role:Secondary
    peer-disk:UpToDate
mkfs rc=0
TARGET     SOURCE        FSTYPE OPTIONS
/mnt/spike /dev/drbd1000 ext4   rw,relatime
1372
```

Изоляция `dv-1` теми же правилами `nft`, через 20 с:

```bash
vm dv-1 "date +%T.%3N; cat /var/tmp/acked; cat /var/tmp/probe.log; ps -eo pid,stat,wchan:32,args | grep -E '[p]robe\.sh|^ *[0-9]+ +[A-Z]+ +[a-z_0-9]* +sync ' || echo no-probe-or-sync; findmnt /mnt/spike"
tb dv-1 "drbdadm status spike"
```

```text
21:54:00.944
21:54:00.982
21:54:21.126
1402
sync: error syncing '/mnt/spike/log': Input/output error
write 1403 failed
no-probe-or-sync
TARGET     SOURCE        FSTYPE OPTIONS
/mnt/spike /dev/drbd1000 ext4   rw,relatime
spike role:Primary
  disk:UpToDate quorum:no open:yes
  lima-dv-2 connection:Connecting
  lima-dv-3 connection:Connecting
```

Первые две метки — до и после `nft add`. `watch.log`:

```text
21:54:00.293 acked=1232 probe=alive:S
21:54:00.802 acked=1366 probe=alive:S
21:54:01.310 acked=1402 probe=alive:S
21:54:12.043 acked=1402 probe=gone
```

`dmesg -T` на `dv-1`:

```text
[Thu Nov 12 21:54:11 2026] drbd spike lima-dv-3: conn( Connected -> NetworkFailure ) peer( Secondary -> Unknown )
[Thu Nov 12 21:54:11 2026] drbd spike/0 drbd1000: quorum( yes -> no )
[Thu Nov 12 21:54:11 2026] Aborting journal on device drbd1000-8.
[Thu Nov 12 21:54:11 2026] Buffer I/O error on dev drbd1000, logical block 65536, lost sync page write
[Thu Nov 12 21:54:11 2026] JBD2: I/O error when updating journal superblock for drbd1000-8.
```

Поведение ext4 после ошибки:

```bash
vm dv-1 "echo x > /mnt/spike/t; echo write rc=\$?; sync /mnt/spike/t; echo fsync rc=\$?; touch /mnt/spike/t2; echo touch rc=\$?; findmnt -o TARGET,OPTIONS /mnt/spike"
```

```text
bash: line 1: /mnt/spike/t: Read-only file system
write rc=1
sync: error opening '/mnt/spike/t': No such file or directory
fsync rc=1
touch: touch rc=1
cannot touch '/mnt/spike/t2': Input/output error
TARGET     OPTIONS
/mnt/spike rw,relatime
```

Точка монтирования остаётся `rw`, но создать файл нельзя: `Read-only file
system` и `Input/output error`.

Монтирование на `dv-2` и сверка:

```text
1402
21:54:33.213
1402
mount rc=0
1402
lines=1402 bad=0
```

Подтверждено 1402, на томе последнее число 1402, 1402 строки без пропусков.

Возврат сети без размонтирования на `dv-1` (21:54:43.7) и опрос
`drbdsetup status spike` на `dv-1`:

```text
21:54:43.743
21:54:43.768
21:54:43 dv-1: spike role:Primary disk:UpToDate quorum:no open:yes lima-dv-2 connection:Connecting lima-dv-3 connection:Connecting
21:54:50 dv-1: spike role:Primary disk:UpToDate quorum:no open:yes lima-dv-2 connection:Connecting lima-dv-3 connection:Connecting
21:54:54 dv-1: spike role:Primary disk:UpToDate quorum:no open:yes lima-dv-2 connection:StandAlone lima-dv-3 connection:StandAlone
...
21:55:30 dv-1: spike role:Primary disk:UpToDate quorum:no open:yes lima-dv-2 connection:StandAlone lima-dv-3 connection:StandAlone
```

`drbdsetup status spike --verbose` в 21:55:33:

```text
== dv-1
spike node-id:0 role:Primary suspended:no force-io-failures:no
  volume:0 minor:1000 disk:UpToDate backing_dev:/dev/ambar/spike quorum:no
      open:yes blocked:no
  lima-dv-2 node-id:1 connection:StandAlone role:Unknown tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Off peer-disk:DUnknown resync-suspended:no
  lima-dv-3 node-id:2 connection:StandAlone role:Unknown tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Off peer-disk:DUnknown resync-suspended:no

== dv-2
spike node-id:1 role:Primary suspended:no force-io-failures:no
  volume:0 minor:1000 disk:UpToDate backing_dev:/dev/ambar/spike quorum:yes
      open:yes blocked:no
  lima-dv-1 node-id:0 connection:StandAlone role:Unknown tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Off peer-disk:DUnknown resync-suspended:no
  lima-dv-3 node-id:2 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
```

`dmesg -T | grep -i -E 'drbd spike|split|StandAlone|primary'` (без потоков
sender/receiver и `Feature flags`), `dv-1`:

```text
[Thu Nov 12 21:54:52 2026] drbd spike lima-dv-2: Handshake to peer 1 successful: Agreed network protocol version 125
[Thu Nov 12 21:54:52 2026] drbd spike: Preparing cluster-wide state change 816854949: 0->1 role( Primary ) conn( Connected )
[Thu Nov 12 21:54:52 2026] drbd spike/0 drbd1000 lima-dv-2: drbd_sync_handshake:
[Thu Nov 12 21:54:52 2026] drbd spike/0 drbd1000 lima-dv-2: self 3FEDAFBAC006A0E8:0000000000000000:0000000000000000:0000000000000000 bits:2 flags:2520
[Thu Nov 12 21:54:52 2026] drbd spike/0 drbd1000 lima-dv-2: peer 199B479DB94A6BDB:3FEDAFBAC006A0E8:3FEDAFBAC006A0E8:0000000000000000 bits:9 flags:2120
[Thu Nov 12 21:54:52 2026] drbd spike/0 drbd1000 lima-dv-2: uuid_compare()=target-use-bitmap by rule=bitmap-peer
[Thu Nov 12 21:54:52 2026] drbd spike/0 drbd1000 lima-dv-2: I shall become SyncTarget, but I am primary!
[Thu Nov 12 21:54:52 2026] drbd spike: Aborting cluster-wide state change 816854949 (16ms) rv = -27
[Thu Nov 12 21:54:52 2026] drbd spike lima-dv-2: conn( Connecting -> Disconnecting ) [connect-failed]
[Thu Nov 12 21:54:52 2026] drbd spike lima-dv-2: Connection closed
[Thu Nov 12 21:54:52 2026] drbd spike lima-dv-2: conn( Disconnecting -> StandAlone ) [disconnected]
[Thu Nov 12 21:54:52 2026] drbd spike lima-dv-3: Handshake to peer 2 successful: Agreed network protocol version 125
[Thu Nov 12 21:54:52 2026] drbd spike: Preparing cluster-wide state change 690769958: 0->2 role( Primary ) conn( Connected )
[Thu Nov 12 21:54:52 2026] drbd spike/0 drbd1000 lima-dv-3: drbd_sync_handshake:
[Thu Nov 12 21:54:52 2026] drbd spike/0 drbd1000 lima-dv-3: self 3FEDAFBAC006A0E8:0000000000000000:0000000000000000:0000000000000000 bits:3 flags:2520
[Thu Nov 12 21:54:52 2026] drbd spike/0 drbd1000 lima-dv-3: peer 199B479DB94A6BDA:3FEDAFBAC006A0E8:3FEDAFBAC006A0E8:0000000000000000 bits:9 flags:2100
[Thu Nov 12 21:54:52 2026] drbd spike/0 drbd1000 lima-dv-3: uuid_compare()=target-use-bitmap by rule=bitmap-peer
[Thu Nov 12 21:54:52 2026] drbd spike/0 drbd1000 lima-dv-3: I shall become SyncTarget, but I am primary!
[Thu Nov 12 21:54:52 2026] drbd spike: Aborting cluster-wide state change 690769958 (28ms) rv = -27
[Thu Nov 12 21:54:52 2026] drbd spike lima-dv-3: conn( Connecting -> Disconnecting ) [connect-failed]
[Thu Nov 12 21:54:52 2026] drbd spike lima-dv-3: Connection closed
[Thu Nov 12 21:54:52 2026] drbd spike lima-dv-3: conn( Disconnecting -> StandAlone ) [disconnected]
```

`dv-2`:

```text
[Thu Nov 12 21:54:52 2026] drbd spike lima-dv-1: Handshake to peer 0 successful: Agreed network protocol version 125
[Thu Nov 12 21:54:52 2026] drbd spike: Preparing remote state change 816854949: 0->1 role( Primary ) conn( Connected )
[Thu Nov 12 21:54:52 2026] drbd spike: State change failed: Multiple primaries not allowed by config (-1)
[Thu Nov 12 21:54:52 2026] drbd spike/0 drbd1000 lima-dv-1: uuid_compare()=source-use-bitmap by rule=bitmap-self
[Thu Nov 12 21:54:52 2026] drbd spike lima-dv-1: Aborting remote state change 816854949
[Thu Nov 12 21:54:52 2026] drbd spike lima-dv-1: conn( Connecting -> Disconnecting ) [receive-disconnect]
[Thu Nov 12 21:54:52 2026] drbd spike lima-dv-1: Connection closed
[Thu Nov 12 21:54:52 2026] drbd spike lima-dv-1: conn( Disconnecting -> StandAlone ) [disconnected]
```

`dv-3` показывает ту же пару строк `uuid_compare()=source-use-bitmap` →
`conn( Disconnecting -> StandAlone )` для `lima-dv-1`. Сообщения
`Split-Brain detected` нет. Образец
`stand/spike/samples/standalone-primary.json` снят на `dv-1`: `Primary`,
`quorum: false`, оба соединения `"connection-state": "StandAlone"`.

Размонтирование на `dv-1` (21:55:47.9) и наблюдение 32 с:

```text
21:55:47.859
umount rc=0
21:55:47.872
21:55:47 dv-1: spike role:Secondary disk:UpToDate quorum:no open:no lima-dv-2 connection:StandAlone lima-dv-3 connection:StandAlone
...
21:56:19 dv-1: spike role:Secondary disk:UpToDate quorum:no open:no lima-dv-2 connection:StandAlone lima-dv-3 connection:StandAlone
```

После размонтирования узел в `Secondary`, но `StandAlone` сам не снимается.
`drbdadm adjust spike` только на `dv-1` (21:56:34.9), через 12 с:

```text
== dv-1
spike role:Secondary
  disk:UpToDate quorum:no open:no
  lima-dv-2 connection:Connecting
  lima-dv-3 connection:Connecting
== dv-2
spike role:Primary
  disk:UpToDate open:yes
  lima-dv-1 connection:StandAlone
  lima-dv-3 role:Secondary
    peer-disk:UpToDate
== dv-3
spike role:Secondary
  disk:UpToDate open:no
  lima-dv-1 connection:StandAlone
  lima-dv-2 role:Primary
    peer-disk:UpToDate
```

`drbdadm adjust spike` на `dv-2` (21:56:49.1) и `dv-3` (21:56:49.7). Вывод
`drbdadm status` после этого не сохранён; конечное состояние подтверждает
только `dmesg -T` на `dv-1` (на `dv-2` и `dv-3` `dmesg` в этот момент не
снимался): обе связи `Connected`, `disk( Inconsistent -> UpToDate )`.

```text
[Thu Nov 12 21:56:35 2026] drbd spike lima-dv-2: conn( StandAlone -> Unconnected ) [connect]
[Thu Nov 12 21:56:35 2026] drbd spike lima-dv-3: conn( StandAlone -> Unconnected ) [connect]
[Thu Nov 12 21:56:50 2026] drbd spike/0 drbd1000 lima-dv-2: uuid_compare()=target-use-bitmap by rule=bitmap-peer
[Thu Nov 12 21:56:50 2026] drbd spike lima-dv-2: conn( Connecting -> Connected ) peer( Unknown -> Primary ) [connected]
[Thu Nov 12 21:56:50 2026] drbd spike/0 drbd1000: disk( UpToDate -> Outdated ) quorum( no -> yes ) [connected]
[Thu Nov 12 21:56:50 2026] drbd spike/0 drbd1000: disk( Outdated -> Inconsistent ) [receive-bitmap]
[Thu Nov 12 21:56:50 2026] drbd spike/0 drbd1000 lima-dv-2: Began resync as SyncTarget (will sync 36 KB [9 bits set]).
[Thu Nov 12 21:56:50 2026] drbd spike/0 drbd1000: disk( Inconsistent -> UpToDate ) [resync-finished]
[Thu Nov 12 21:56:50 2026] drbd spike/0 drbd1000 lima-dv-3: uuid_compare()=no-sync by rule=reconnected
[Thu Nov 12 21:56:50 2026] drbd spike lima-dv-3: conn( Connecting -> Connected ) peer( Unknown -> Secondary ) [connected]
```

| Вывод E1 (один прогон) | Наблюдение |
|---|---|
| `StandAlone` при `io-error` | да, если узел в роли `Primary` в момент возврата сети: `I shall become SyncTarget, but I am primary!` на обеих связях |
| Снимается ли сам | нет: ни за ~56 с до размонтирования (21:54:52 → 21:55:47.9), ни за 32 с после |
| Что восстановило | проверен один порядок: размонтирование на `dv-1`, затем `drbdadm adjust spike` на трёх узлах; `adjust` только на `dv-1` недостаточен; `adjust` без размонтирования не пробовали |
| Данные | досылка 36 КиБ по битовой карте на `dv-1`; подтверждённые записи на месте (1402 = 1402) |
| Отличие от split-brain | `Split-Brain detected` в `dmesg` нет, отбрасывать данные не понадобилось |

### Опыт E2. Принудительная остановка узла с копией при писателе на стороне без диска

`limactl stop --help` содержит `-f, --force  Force stop the instance`. Ресурс
`spike2` создан заново (`res.sh spike2 1001 7001 dv-3 io-error`, затем
`new-current-uuid --clear-bitmap`, `mkfs.ext4`), писатель и `watch.sh`
запущены на `dv-3`, `spike` в это время поднят без записи. Один прогон.

```bash
tb dv-1 "drbdsetup show spike2 --show-defaults | grep -E 'ping|timeout|connect-int|ko-count'"
```

```text
        twopc-timeout   	300; # 1/10 seconds, default
        twopc-retry-timeout	1; # 1/10 seconds, default
        auto-promote-timeout	20; # 1/10 seconds, default
                disk-timeout    	0; # 1/10 seconds, default
            timeout         	60; # 1/10 seconds, default
            connect-int     	10; # seconds, default
            ping-int        	10; # seconds, default
            ko-count        	7; # default
            ping-timeout    	5; # 1/10 seconds, default
            socket-check-timeout	0; # default
```

Строки `timeout` … `socket-check-timeout` повторяются для второго соединения с
теми же значениями.

```bash
vm dv-3 "date +%T.%3N"; limactl stop --force dv-2; echo stop rc=$?; vm dv-3 "date +%T.%3N"
```

```text
21:57:46.074
time="2026-11-12T21:57:46+08:00" level=info msg="Sending SIGKILL to the vz driver process 11700"
time="2026-11-12T21:57:46+08:00" level=info msg="Sending SIGKILL to the host agent process 11700"
time="2026-11-12T21:57:46+08:00" level=info msg="Removing *.pid *.sock *.tmp under `/Users/<user>/.lima/dv-2`"
...
stop rc=0
21:57:46.280
```

`limactl list` после остановки: `dv-2 Stopped`. `watch.log` на `dv-3`:

```text
21:57:45.295 acked=5206 probe=alive:S
21:57:45.803 acked=5408 probe=alive:S
21:57:46.313 acked=5533 probe=alive:S
21:57:46.821 acked=5533 probe=alive:S
21:57:47.336 acked=5533 probe=alive:S
21:57:47.846 acked=5533 probe=alive:S
21:57:48.360 acked=5533 probe=alive:S
21:57:48.871 acked=5533 probe=alive:S
21:57:49.381 acked=5533 probe=alive:S
21:57:49.894 acked=5533 probe=alive:S
21:57:50.405 acked=5533 probe=alive:S
21:57:50.916 acked=5533 probe=alive:S
21:57:51.426 acked=5643 probe=alive:R
21:57:51.938 acked=5777 probe=alive:S
```

`dmesg -T` на `dv-3` (без потоков sender/receiver):

```text
[Thu Nov 12 21:57:48 2026] drbd spike lima-dv-2: PingAck did not arrive in time.
[Thu Nov 12 21:57:48 2026] drbd spike lima-dv-2: conn( Connected -> NetworkFailure ) peer( Secondary -> Unknown )
[Thu Nov 12 21:57:50 2026] drbd spike2: Preparing remote state change 1267450917: 0->all empty
[Thu Nov 12 21:57:51 2026] drbd spike2 lima-dv-2: PingAck did not arrive in time.
[Thu Nov 12 21:57:51 2026] drbd spike2 lima-dv-2: conn( Connected -> NetworkFailure ) peer( Secondary -> Unknown )
[Thu Nov 12 21:57:51 2026] drbd spike2/0 drbd1001 lima-dv-2: pdsk( UpToDate -> DUnknown ) repl( Established -> Off )
[Thu Nov 12 21:57:51 2026] drbd spike2 lima-dv-1: Aborting remote state change 1267450917
[Thu Nov 12 21:57:51 2026] drbd spike2: Preparing cluster-wide state change 1642881236: 2->all empty
[Thu Nov 12 21:57:51 2026] drbd spike2: Committing cluster-wide state change 1642881236 (0ms)
[Thu Nov 12 21:57:51 2026] drbd spike2/0 drbd1001: sending new current UUID: 8FC085E5ED3673B7
[Thu Nov 12 21:57:51 2026] drbd spike2 lima-dv-2: conn( NetworkFailure -> Unconnected ) [disconnected]
[Thu Nov 12 21:57:51 2026] drbd spike2 lima-dv-2: conn( Unconnected -> Connecting ) [connecting]
[Thu Nov 12 21:57:51 2026] drbd spike2/0 drbd1001: rotated data generation confirmed durable in a quorate partition (gen 8FC085E5ED3673B7)
```

`dmesg -T` на `dv-1`:

```text
[Thu Nov 12 21:57:47 2026] drbd spike lima-dv-2: PingAck did not arrive in time.
[Thu Nov 12 21:57:47 2026] drbd spike lima-dv-2: conn( Connected -> NetworkFailure ) peer( Secondary -> Unknown )
[Thu Nov 12 21:57:50 2026] drbd spike2 lima-dv-2: PingAck did not arrive in time.
[Thu Nov 12 21:57:50 2026] drbd spike2/0 drbd1001: Would lose quorum, but using tiebreaker logic to keep
[Thu Nov 12 21:57:50 2026] drbd spike2 lima-dv-2: conn( Connected -> NetworkFailure ) peer( Secondary -> Unknown )
[Thu Nov 12 21:57:50 2026] drbd spike2/0 drbd1001 lima-dv-2: pdsk( UpToDate -> DUnknown ) repl( Established -> Off )
[Thu Nov 12 21:57:51 2026] drbd spike2: Retrying cluster-wide state change after 112ms
[Thu Nov 12 21:57:51 2026] drbd spike2/0 drbd1001 lima-dv-3: received new current UUID: 8FC085E5ED3673B7 weak_nodes=FFFFFFFFFFFFFFFE
```

Строки `quorum( yes -> no )` для `spike2` за время опыта нет ни на `dv-1`, ни
на `dv-3` (`dmesg -T | grep spike2 | grep 'quorum('`: последние записи —
`quorum( no -> yes )` в 21:57:18/21:57:19 при создании ресурса). Та же строка
`Would lose quorum, but using tiebreaker logic to keep` есть на `dv-1` и в
шаге 8 (21:41:55). Образец `stand/spike/samples/diskless-peer-killed.json`
снят на `dv-3` после остановки: один пир `Connecting`, `quorum: true`.

| Величина (E2) | Значение |
|---|---|
| Последний рост номера → следующий рост | 21:57:46.313 → 21:57:51.426, 5.1 с между выборками; с учётом шага 0.5 с — от 4.6 до 5.6 с |
| Остановка → `PingAck did not arrive in time` для `spike2` | ~4–6 с: на `dv-3` 4.7–5.9 с (остановка 21:57:46.1–46.3, сообщение в 21:57:51, `dmesg -T` с точностью до секунды); на `dv-1` сообщение в 21:57:50 |
| Связь с параметрами | `ping-int 10` с, `ping-timeout 0.5` с, `timeout 6` с; пауза короче `ping-int` и `timeout` [проверить: какой механизм DRBD отправляет ping раньше `ping-int` при ожидающей записи] |
| Кворум | сохранён на `dv-3` и `dv-1` |

Остановка писателя и сверка. Писатель работал на `dv-3` с томом `spike2`
(сторона без диска), точка монтирования `/mnt/spike2`; журнал читался с этой
точки монтирования до `umount`:

```text
stopped
acked2=24830
24831
lines=24831 bad=0
umount rc=0
```

Подтверждено 24830, на томе последнее число 24831, 24831 строка без пропусков.

Возврат `dv-2`:

```bash
stand/up.sh
kc get node lima-dv-2; kc -n ambar-spike rollout status ds/toolbox --timeout=180s
tb dv-2 "/opt/ambar/load-module.sh"
vm dv-2 "mkdir -p /dev/ambar; for r in spike spike2; do f=/var/lib/rancher/ambar/volumes/\$r.img; ln -sfn \$(losetup --find --show --direct-io=on \$f) /dev/ambar/\$r; done; losetup -l -O NAME,BACK-FILE,DIO"
tb dv-2 "drbdadm up spike; drbdadm up spike2; for i in \$(seq 1 40); do …drbdsetup status… ; sleep 0.25; done" | uniq -f1
```

```text
OK: stand is up
up rc=0
21:59:10 lima-dv-2   Ready   control-plane,etcd   39m   v1.37.2+k3s1
daemon set "toolbox" successfully rolled out
toolbox-g9ckc   1/1     Running   2 (16s ago)   29m   192.168.104.3   lima-dv-2   <none>           <none>
loaded drbd 9.3.4 for kernel 6.12.116+deb13-arm64
load rc=0
NAME       BACK-FILE                                  DIO
/dev/loop1 /var/lib/rancher/ambar/volumes/spike2.img   1
/dev/loop0 /var/lib/rancher/ambar/volumes/spike.img    1
21:59:20.971
13:59:21.707 spike role:Secondary disk:UpToDate quorum:no open:no  | spike2 role:Secondary disk:UpToDate quorum:no open:no lima-dv-1 connection:Connecting lima-dv-3 connection:Connecting
13:59:22.491 spike role:Secondary disk:UpToDate open:no  | spike2 role:Secondary disk:Inconsistent open:no lima-dv-1 role:Secondary replication:SyncTarget peer-disk:UpToDate done:55.93 lima-dv-3 role:Secondary peer-disk:Diskless peer-client:yes resync-suspended:dependency
13:59:22.755 spike role:Secondary disk:UpToDate open:no  | spike2 role:Secondary disk:UpToDate open:no lima-dv-1 role:Secondary peer-disk:UpToDate lima-dv-3 role:Secondary peer-disk:Diskless peer-client:yes
```

Метки `13:59` — время пода (UTC), соответствуют 21:59 на узлах. Узел вернулся
с первой попытки.

`dmesg -T | grep -i drbd` на `dv-2`, от загрузки модуля до конца
досылки:

```text
[Thu Nov 12 21:59:20 2026] drbd: loading out-of-tree module taints kernel.
[Thu Nov 12 21:59:20 2026] drbd: module verification failed: signature and/or required key missing - tainting kernel
[Thu Nov 12 21:59:20 2026] drbd: initialized. Version: 9.3.4 (api:2/proto:118-125)
[Thu Nov 12 21:59:20 2026] drbd: GIT-hash: e59b287b199f7224468af85b68a10e83c68213f5 build by @buildkitsandbox, 2026-11-12 13:05:27
[Thu Nov 12 21:59:20 2026] drbd: registered as block device major 147
[Thu Nov 12 21:59:20 2026] drbd: registered transport class 'tcp' (version:9.3.4)
[Thu Nov 12 21:59:21 2026] drbd spike: Starting worker thread (node-id 1)
[Thu Nov 12 21:59:21 2026] drbd spike lima-dv-1: Starting sender thread (peer-node-id 0)
[Thu Nov 12 21:59:21 2026] drbd spike lima-dv-3: Starting sender thread (peer-node-id 2)
[Thu Nov 12 21:59:21 2026] drbd spike/0 drbd1000: meta-data IO uses: blk-bio
[Thu Nov 12 21:59:21 2026] drbd spike/0 drbd1000: Meta data written without the bitmap provenance feature; all out-of-sync bits toward node(s) 0x5 count as set with reason
[Thu Nov 12 21:59:21 2026] drbd spike/0 drbd1000: disk( Diskless -> Attaching ) [attach]
[Thu Nov 12 21:59:21 2026] drbd spike/0 drbd1000: Maximum number of peer devices = 2
[Thu Nov 12 21:59:21 2026] drbd spike: Method to ensure write ordering: flush
[Thu Nov 12 21:59:21 2026] drbd spike/0 drbd1000: Allocating 64 KiB for new bitmap
[Thu Nov 12 21:59:21 2026] drbd spike/0 drbd1000: resync bitmap: bits=262119 bits_4k=262119 words=8192 pages=16
[Thu Nov 12 21:59:21 2026] drbd spike/0 drbd1000: was UpToDate, new region assumed zeroed
[Thu Nov 12 21:59:21 2026] drbd1000: detected capacity change from 0 to 2096952
[Thu Nov 12 21:59:21 2026] drbd spike/0 drbd1000: size = 1024 MB (1048476 KB)
[Thu Nov 12 21:59:21 2026] drbd spike/0 drbd1000: disk( Attaching -> UpToDate ) [attach]
[Thu Nov 12 21:59:21 2026] drbd spike/0 drbd1000: attached to current UUID: 199B479DB94A6BDB
[Thu Nov 12 21:59:21 2026] drbd spike/0 drbd1000: Setting exposed data uuid: 199B479DB94A6BDA
[Thu Nov 12 21:59:21 2026] drbd spike lima-dv-1: conn( StandAlone -> Unconnected ) [connect]
[Thu Nov 12 21:59:21 2026] drbd spike lima-dv-1: Starting receiver thread (peer-node-id 0)
[Thu Nov 12 21:59:21 2026] drbd spike lima-dv-1: conn( Unconnected -> Connecting ) [connecting]
[Thu Nov 12 21:59:21 2026] drbd spike lima-dv-3: conn( StandAlone -> Unconnected ) [connect]
[Thu Nov 12 21:59:21 2026] drbd spike lima-dv-3: Starting receiver thread (peer-node-id 2)
[Thu Nov 12 21:59:21 2026] drbd spike lima-dv-3: conn( Unconnected -> Connecting ) [connecting]
[Thu Nov 12 21:59:21 2026] drbd spike2: Starting worker thread (node-id 1)
[Thu Nov 12 21:59:21 2026] drbd spike2 lima-dv-1: Starting sender thread (peer-node-id 0)
[Thu Nov 12 21:59:21 2026] drbd spike2 lima-dv-3: Starting sender thread (peer-node-id 2)
[Thu Nov 12 21:59:21 2026] drbd spike2/0 drbd1001: meta-data IO uses: blk-bio
[Thu Nov 12 21:59:21 2026] drbd spike2/0 drbd1001: Meta data written without the bitmap provenance feature; all out-of-sync bits toward node(s) 0x1 count as set with reason
[Thu Nov 12 21:59:21 2026] drbd spike2/0 drbd1001: disk( Diskless -> Attaching ) [attach]
[Thu Nov 12 21:59:21 2026] drbd spike2/0 drbd1001: Maximum number of peer devices = 1
[Thu Nov 12 21:59:21 2026] drbd spike2: Method to ensure write ordering: flush
[Thu Nov 12 21:59:21 2026] drbd spike2/0 drbd1001: Allocating 32 KiB for new bitmap
[Thu Nov 12 21:59:21 2026] drbd spike2/0 drbd1001: resync bitmap: bits=262127 bits_4k=262127 words=4096 pages=8
[Thu Nov 12 21:59:21 2026] drbd spike2/0 drbd1001: was UpToDate, new region assumed zeroed
[Thu Nov 12 21:59:21 2026] drbd1001: detected capacity change from 0 to 2097016
[Thu Nov 12 21:59:21 2026] drbd spike2/0 drbd1001: size = 1024 MB (1048508 KB)
[Thu Nov 12 21:59:21 2026] drbd spike2/0 drbd1001: disk( Attaching -> UpToDate ) [attach]
[Thu Nov 12 21:59:21 2026] drbd spike2/0 drbd1001: attached to current UUID: 960C07E5757CF0BA
[Thu Nov 12 21:59:21 2026] drbd spike2/0 drbd1001: Setting exposed data uuid: 960C07E5757CF0BA
[Thu Nov 12 21:59:21 2026] drbd spike2 lima-dv-1: conn( StandAlone -> Unconnected ) [connect]
[Thu Nov 12 21:59:21 2026] drbd spike2 lima-dv-3: conn( StandAlone -> Unconnected ) [connect]
[Thu Nov 12 21:59:21 2026] drbd spike2 lima-dv-1: Starting receiver thread (peer-node-id 0)
[Thu Nov 12 21:59:21 2026] drbd spike2 lima-dv-1: conn( Unconnected -> Connecting ) [connecting]
[Thu Nov 12 21:59:21 2026] drbd spike2 lima-dv-3: Starting receiver thread (peer-node-id 2)
[Thu Nov 12 21:59:21 2026] drbd spike2 lima-dv-3: conn( Unconnected -> Connecting ) [connecting]
[Thu Nov 12 21:59:22 2026] drbd spike lima-dv-1: Handshake to peer 0 successful: Agreed network protocol version 125
[Thu Nov 12 21:59:22 2026] drbd spike lima-dv-1: Feature flags enabled on protocol level: 0x7ff TRIM THIN_RESYNC WRITE_SAME WRITE_ZEROES RESYNC_DAGTAG WRITE_POSTPONE
[Thu Nov 12 21:59:22 2026] drbd spike lima-dv-3: Handshake to peer 2 successful: Agreed network protocol version 125
[Thu Nov 12 21:59:22 2026] drbd spike lima-dv-3: Feature flags enabled on protocol level: 0x7ff TRIM THIN_RESYNC WRITE_SAME WRITE_ZEROES RESYNC_DAGTAG WRITE_POSTPONE
[Thu Nov 12 21:59:22 2026] drbd spike: Preparing cluster-wide state change 2828621637: 1->2 role( Secondary ) conn( Connected )
[Thu Nov 12 21:59:22 2026] drbd spike lima-dv-1: Aborting local state change 2828621637 to yield to remote state change 346482959.
[Thu Nov 12 21:59:22 2026] drbd spike: Aborting cluster-wide state change 2828621637 (0ms) rv = -19
[Thu Nov 12 21:59:22 2026] drbd spike: Preparing remote state change 346482959: 0->1 role( Secondary ) conn( Connected )
[Thu Nov 12 21:59:22 2026] drbd spike/0 drbd1000 lima-dv-1: drbd_sync_handshake:
[Thu Nov 12 21:59:22 2026] drbd spike/0 drbd1000 lima-dv-1: self 199B479DB94A6BDA:0000000000000000:3FEDAFBAC006A0E8:0000000000000000 bits:0 flags:20
[Thu Nov 12 21:59:22 2026] drbd spike/0 drbd1000 lima-dv-1: peer 199B479DB94A6BDA:0000000000000000:0000000000000000:0000000000000000 bits:0 flags:120
[Thu Nov 12 21:59:22 2026] drbd spike/0 drbd1000 lima-dv-1: uuid_compare()=no-sync by rule=lost-quorum
[Thu Nov 12 21:59:22 2026] drbd spike lima-dv-1: Aborting remote state change 346482959
[Thu Nov 12 21:59:22 2026] drbd spike2 lima-dv-1: Handshake to peer 0 successful: Agreed network protocol version 125
[Thu Nov 12 21:59:22 2026] drbd spike2 lima-dv-1: Feature flags enabled on protocol level: 0x7ff TRIM THIN_RESYNC WRITE_SAME WRITE_ZEROES RESYNC_DAGTAG WRITE_POSTPONE
[Thu Nov 12 21:59:22 2026] drbd spike2 lima-dv-3: Handshake to peer 2 successful: Agreed network protocol version 125
[Thu Nov 12 21:59:22 2026] drbd spike2 lima-dv-3: Feature flags enabled on protocol level: 0x7ff TRIM THIN_RESYNC WRITE_SAME WRITE_ZEROES RESYNC_DAGTAG WRITE_POSTPONE
[Thu Nov 12 21:59:22 2026] drbd spike2: Preparing remote state change 1489527815: 0->1 role( Secondary ) conn( Connected )
[Thu Nov 12 21:59:22 2026] drbd spike2/0 drbd1001 lima-dv-1: drbd_sync_handshake:
[Thu Nov 12 21:59:22 2026] drbd spike2/0 drbd1001 lima-dv-1: self 960C07E5757CF0BA:0000000000000000:209006FEC8C9AFDA:0000000000000000 bits:0 flags:20
[Thu Nov 12 21:59:22 2026] drbd spike2/0 drbd1001 lima-dv-1: peer 8FC085E5ED3673B6:960C07E5757CF0BA:209006FEC8C9AFDA:0000000000000000 bits:4130 flags:2120
[Thu Nov 12 21:59:22 2026] drbd spike2/0 drbd1001 lima-dv-1: uuid_compare()=target-use-bitmap by rule=bitmap-peer
[Thu Nov 12 21:59:22 2026] drbd spike2 lima-dv-1: Committing remote state change 1489527815 (primary_nodes=0)
[Thu Nov 12 21:59:22 2026] drbd spike2 lima-dv-1: conn( Connecting -> Connected ) peer( Unknown -> Secondary ) [remote]
[Thu Nov 12 21:59:22 2026] drbd spike2/0 drbd1001: disk( UpToDate -> Outdated ) quorum( no -> yes ) [remote]
[Thu Nov 12 21:59:22 2026] drbd spike2/0 drbd1001 lima-dv-1: pdsk( DUnknown -> UpToDate ) repl( Off -> WFBitMapT ) [remote]
[Thu Nov 12 21:59:22 2026] drbd spike2/0 drbd1001: Setting exposed data uuid: 8FC085E5ED3673B6
[Thu Nov 12 21:59:22 2026] drbd spike2: Preparing cluster-wide state change 2062527967: 1->2 role( Secondary ) conn( Connected )
[Thu Nov 12 21:59:22 2026] drbd spike2/0 drbd1001 lima-dv-1: receive bitmap stats [Bytes(packets)]: plain 0(0), RLE 46(1), total 46; compression: 99.9%
[Thu Nov 12 21:59:22 2026] drbd spike2/0 drbd1001 lima-dv-1: send bitmap stats [Bytes(packets)]: plain 0(0), RLE 46(1), total 46; compression: 99.9%
[Thu Nov 12 21:59:22 2026] drbd spike: Preparing cluster-wide state change 727846996: 1->2 role( Secondary ) conn( Connected )
[Thu Nov 12 21:59:22 2026] drbd spike2/0 drbd1001 lima-dv-3: self 960C07E5757CF0BA:0000000000000000:209006FEC8C9AFDA:0000000000000000 bits:0 flags:0
[Thu Nov 12 21:59:22 2026] drbd spike2/0 drbd1001 lima-dv-3: peer's exposed UUID: 8FC085E5ED3673B7
[Thu Nov 12 21:59:22 2026] drbd spike2: State change 2062527967: primary_nodes=0, weak_nodes=0
[Thu Nov 12 21:59:22 2026] drbd spike2: Committing cluster-wide state change 2062527967 (16ms)
[Thu Nov 12 21:59:22 2026] drbd spike2 lima-dv-3: conn( Connecting -> Connected ) peer( Unknown -> Secondary ) [connected]
[Thu Nov 12 21:59:22 2026] drbd spike2/0 drbd1001 lima-dv-3: pdsk( DUnknown -> Diskless ) repl( Off -> Established ) [connected]
[Thu Nov 12 21:59:22 2026] drbd spike/0 drbd1000 lima-dv-3: drbd_sync_handshake:
[Thu Nov 12 21:59:22 2026] drbd spike/0 drbd1000 lima-dv-3: self 199B479DB94A6BDA:0000000000000000:3FEDAFBAC006A0E8:0000000000000000 bits:0 flags:20
[Thu Nov 12 21:59:22 2026] drbd spike/0 drbd1000 lima-dv-3: peer 199B479DB94A6BDA:0000000000000000:3FEDAFBAC006A0E8:0000000000000000 bits:0 flags:120
[Thu Nov 12 21:59:22 2026] drbd spike/0 drbd1000 lima-dv-3: uuid_compare()=no-sync by rule=lost-quorum
[Thu Nov 12 21:59:22 2026] drbd spike: State change 727846996: primary_nodes=0, weak_nodes=0
[Thu Nov 12 21:59:22 2026] drbd spike: Committing cluster-wide state change 727846996 (20ms)
[Thu Nov 12 21:59:22 2026] drbd spike lima-dv-3: conn( Connecting -> Connected ) peer( Unknown -> Secondary ) [connected]
[Thu Nov 12 21:59:22 2026] drbd spike/0 drbd1000: quorum( no -> yes ) [connected]
[Thu Nov 12 21:59:22 2026] drbd spike/0 drbd1000 lima-dv-3: pdsk( DUnknown -> UpToDate ) repl( Off -> Established ) [connected]
[Thu Nov 12 21:59:22 2026] drbd spike/0 drbd1000 lima-dv-3: cleared bm UUID and bitmap 199B479DB94A6BDA:0000000000000000:3FEDAFBAC006A0E8:0000000000000000
[Thu Nov 12 21:59:22 2026] drbd spike: Preparing remote state change 1618562076: 0->1 role( Secondary ) conn( Connected )
[Thu Nov 12 21:59:22 2026] drbd spike/0 drbd1000 lima-dv-1: drbd_sync_handshake:
[Thu Nov 12 21:59:22 2026] drbd spike/0 drbd1000 lima-dv-1: self 199B479DB94A6BDA:0000000000000000:3FEDAFBAC006A0E8:0000000000000000 bits:0 flags:20
[Thu Nov 12 21:59:22 2026] drbd spike/0 drbd1000 lima-dv-1: peer 199B479DB94A6BDA:0000000000000000:0000000000000000:0000000000000000 bits:0 flags:120
[Thu Nov 12 21:59:22 2026] drbd spike/0 drbd1000 lima-dv-1: uuid_compare()=no-sync by rule=lost-quorum
[Thu Nov 12 21:59:22 2026] drbd spike lima-dv-1: Committing remote state change 1618562076 (primary_nodes=0)
[Thu Nov 12 21:59:22 2026] drbd spike lima-dv-1: conn( Connecting -> Connected ) peer( Unknown -> Secondary ) [remote]
[Thu Nov 12 21:59:22 2026] drbd spike/0 drbd1000 lima-dv-1: pdsk( DUnknown -> UpToDate ) repl( Off -> Established ) [remote]
[Thu Nov 12 21:59:22 2026] drbd spike/0 drbd1000 lima-dv-1: cleared bm UUID and bitmap 199B479DB94A6BDA:0000000000000000:3FEDAFBAC006A0E8:0000000000000000
[Thu Nov 12 21:59:22 2026] drbd spike2/0 drbd1001: disk( Outdated -> Inconsistent ) [postponed-resync]
[Thu Nov 12 21:59:22 2026] drbd spike2/0 drbd1001 lima-dv-3: resync-susp( no -> connection dependency ) peer_replication( yes -> no ) [postponed-resync]
[Thu Nov 12 21:59:22 2026] drbd spike2/0 drbd1001 lima-dv-1: repl( WFBitMapT -> SyncTarget ) peer_replication( yes -> no ) [postponed-resync]
[Thu Nov 12 21:59:22 2026] drbd spike2/0 drbd1001 lima-dv-1: Began resync as SyncTarget (will sync 16520 KB [4130 bits set]).
[Thu Nov 12 21:59:22 2026] drbd spike2/0 drbd1001 lima-dv-3: peer_replication( no -> yes ) [enable-peer-replication]
[Thu Nov 12 21:59:22 2026] drbd spike2/0 drbd1001 lima-dv-1: peer_replication( no -> yes ) [enable-peer-replication]
[Thu Nov 12 21:59:22 2026] drbd spike2/0 drbd1001 lima-dv-1: Resync done (total 1 sec; paused 0 sec; 16520 K/sec)
[Thu Nov 12 21:59:22 2026] drbd spike2/0 drbd1001 lima-dv-1: updated UUIDs 8FC085E5ED3673B6:0000000000000000:209006FEC8C9AFDA:0000000000000000
[Thu Nov 12 21:59:22 2026] drbd spike2/0 drbd1001: disk( Inconsistent -> UpToDate ) [resync-finished]
[Thu Nov 12 21:59:22 2026] drbd spike2/0 drbd1001 lima-dv-3: resync-susp( connection dependency -> no ) [resync-finished]
[Thu Nov 12 21:59:22 2026] drbd spike2/0 drbd1001 lima-dv-1: repl( SyncTarget -> Established ) [resync-finished]
```

| Ресурс на `dv-2` | Последовательность состояний диска |
|---|---|
| `spike2` (на томе была запись за время отсутствия) | `Diskless` → `Attaching` → `UpToDate` (`[attach]`), при соединении `UpToDate` → `Outdated` → `Inconsistent` → `UpToDate`; досылка 16520 КиБ по битовой карте за 1 с |
| `spike` (записи не было) | `Diskless` → `Attaching` → `UpToDate` (`[attach]`), `uuid_compare()=no-sync`, досылки нет |

Последовательности для изолированного узла (шаги 6, 7 и E1, `dv-1`):
`UpToDate` → `Outdated` → `Inconsistent` → `UpToDate`.

### Очистка стенда после опытов

```bash
stand/spike/clean.sh
stand/check.sh
kc get nodes --no-headers
for n in dv-1 dv-2 dv-3; do vm $n "cat /sys/module/drbd/version; ps -eo pid,comm,args | grep -E '[p]robe\.sh|[w]atch\.sh' || echo 'ps: no probe/watch'; losetup -l | wc -l; ls /dev/ambar; ls /usr/local/bin /var/tmp; nft list tables | grep spike || echo no-spike-table"; done
```

```text
== dv-1
# No currently configured DRBD found.
/var/lib/rancher/ambar/res:

/var/lib/rancher/ambar/volumes:
0
no probe/watch processes
...
clean rc=0
OK: stand is up
lima-dv-1   Ready   control-plane,etcd   40m   v1.37.2+k3s1
lima-dv-2   Ready   control-plane,etcd   40m   v1.37.2+k3s1
lima-dv-3   Ready   control-plane,etcd   39m   v1.37.2+k3s1
== dv-1
9.3.4
ps: no probe/watch
0
ls: cannot access '/dev/ambar': No such file or directory
no-spike-table
== dv-2
9.3.4
ps: no probe/watch
0
ls: cannot access '/dev/ambar': No such file or directory
no-spike-table
== dv-3
9.3.4
ps: no probe/watch
0
ls: cannot access '/dev/ambar': No such file or directory
no-spike-table
```

Вывод `ls /usr/local/bin /var/tmp` на каждом узле: в `/usr/local/bin` только
`crictl ctr k3s k3s-killall.sh k3s-uninstall.sh kubectl lima-guestagent`, в
`/var/tmp` только `cloud-init` и каталог `systemd-private-…`; `probe.sh`,
`watch.sh`, `fsync-bench.sh`, файлы `acked*`, `probe*.log`, `watch*.log` удалены.

### Опыт C1. Устаревшая копия и узел без диска после последовательных отказов

Вопрос: при двух копиях и узле без диска может ли кластер подняться на
устаревшей копии, принять запись и потерять подтверждённые данные.

Ресурс `spike2` (`dv-1` и `dv-2` — копии, `dv-3` — узел без диска), по одному
прогону на вариант. Команды DRBD выполнялись через `stand/spike/ctr.sh`
(контейнер toolbox через `k3s crictl exec`, без API). Метки `14:…` — время
контейнера (UTC), на узлах +08.

```bash
stand/spike/ctr.sh dv-3 'hostname; drbdadm --version | grep KERNEL_VERSION=; drbdsetup status'
```

```text
lima-dv-3
DRBD_KERNEL_VERSION=9.3.4
# No currently configured DRBD found.
```

#### Подготовка (оба варианта одинаково)

```bash
stand/spike/res.sh spike2 1001 7001 dv-3 io-error
stand/spike/ctr.sh dv-1 "drbdadm new-current-uuid --clear-bitmap spike2/0"
stand/spike/ctr.sh dv-3 "mkfs.ext4 -q /dev/drbd1001"
vm dv-3 "mkdir -p /mnt/spike2 && mount /dev/drbd1001 /mnt/spike2"
# probe.sh /mnt/spike2 /var/tmp/acked2 и watch.sh /var/tmp/acked2 на dv-3, как в шаге 8
```

После `new-current-uuid` на трёх узлах `UpToDate`/`UpToDate`/`Diskless`, на
`dv-3` после `mount` — `role:Primary disk:Diskless client:yes open:yes`.

#### Вариант A, шаг 1. Принудительная остановка `dv-2`, запись продолжается

```bash
vm dv-3 "date +%T.%3N; cat /var/tmp/acked2"; limactl stop --force dv-2; vm dv-3 "date +%T.%3N"
sleep 25; vm dv-3 "date +%T.%3N; cat /var/tmp/acked2"
```

```text
22:30:46.167
1908
stop rc=0
22:30:46.426
22:31:11.660
6717
```

`watch2.log` на `dv-3` (повторы свёрнуты):

```text
22:30:46.046 acked=1896 probe=alive:S
22:30:46.558 acked=1919 probe=alive:S
...
22:30:57.314 acked=1919 probe=alive:S
22:30:57.825 acked=2017 probe=alive:S
```

Запись стояла 11.3 с между выборками (22:30:46.558 → 22:30:57.825), в E2 —
5.1 с. `dmesg -T` на `dv-1`:

```text
[Thu Nov 12 22:30:56 2026] drbd spike2 lima-dv-2: PingAck did not arrive in time.
[Thu Nov 12 22:30:56 2026] drbd spike2/0 drbd1001: Would lose quorum, but using tiebreaker logic to keep
[Thu Nov 12 22:30:56 2026] drbd spike2 lima-dv-2: conn( Connected -> NetworkFailure ) peer( Secondary -> Unknown )
[Thu Nov 12 22:30:57 2026] drbd spike2/0 drbd1001 lima-dv-3: received new current UUID: B64703DC5853FA4D weak_nodes=FFFFFFFFFFFFFFFE
```

`dv-3`:

```text
[Thu Nov 12 22:30:57 2026] drbd spike2 lima-dv-2: PingAck did not arrive in time.
[Thu Nov 12 22:30:57 2026] drbd spike2/0 drbd1001: sending new current UUID: B64703DC5853FA4D
[Thu Nov 12 22:30:57 2026] drbd spike2/0 drbd1001: rotated data generation confirmed durable in a quorate partition (gen B64703DC5853FA4D)
```

Остановка писателя, сверка и `umount` на `dv-3`:

```text
22:31:19.423
probe-stopped
acked2=9348
9349
lines=9349 bad=0
umount rc=0
22:31:20.459
```

ACKED = 9348, на томе последнее число 9349, 9349 строк без пропусков.
Идентификаторы поколений на `dv-1` (`drbdadm get-gi spike2/0`, первая строка —
к `dv-2`, вторая — к `dv-3`):

```text
B64703DC5853FA4C:08136E27BB72DA48:8755DFAC880D4BBA:858046A0E44F38C2:1:1:0:0:0:0:0:0:0:0:0:1:1
B64703DC5853FA4C:0000000000000000:8755DFAC880D4BBA:858046A0E44F38C2:1:1:0:0:0:0:0:0:0:0:0:0:0
```

#### Вариант A, шаг 2. Выключена новейшая копия, поднята устаревшая

```bash
vm dv-3 "date +%T.%3N"; limactl stop --force dv-1; vm dv-3 "date +%T.%3N"
sleep 15; stand/spike/ctr.sh dv-3 "drbdsetup status spike2 --verbose"
kc get nodes
```

```text
22:31:54.196
stop rc=0
22:31:54.373
spike2 node-id:2 role:Secondary suspended:no force-io-failures:no
  volume:0 minor:1001 disk:Diskless client:yes backing_dev:none quorum:no open:no
      blocked:no
  lima-dv-1 node-id:0 connection:Connecting role:Unknown tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Off peer-disk:DUnknown resync-suspended:no
  lima-dv-2 node-id:1 connection:Connecting role:Unknown tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Off peer-disk:DUnknown resync-suspended:no
kc: no running node with a reachable API server among: dv-1 dv-2 dv-3
```

`dmesg -T` на `dv-3`:

```text
[Thu Nov 12 22:32:01 2026] drbd spike2 lima-dv-1: PingAck did not arrive in time.
[Thu Nov 12 22:32:01 2026] drbd spike2 lima-dv-1: conn( Connected -> NetworkFailure ) peer( Secondary -> Unknown )
[Thu Nov 12 22:32:01 2026] drbd spike2/0 drbd1001: quorum( yes -> no )
```

Образец `stand/spike/samples/diskless-alone-no-quorum.json`.

Запуск `dv-2` (22:32:21 → `READY` в 22:32:43; API ответил сразу, в etcd
два узла из трёх), модуль, loop, `drbdadm up`:

```bash
limactl start --tty=false dv-2
stand/spike/ctr.sh dv-2 "/opt/ambar/load-module.sh"
vm dv-2 "mkdir -p /dev/ambar; f=/var/lib/rancher/ambar/volumes/spike2.img; ln -sfn \$(losetup --find --show --direct-io=on \$f) /dev/ambar/spike2"
stand/spike/ctr.sh dv-2 "drbdadm up spike2; …опрос drbdsetup status…"
```

```text
loaded drbd 9.3.4 for kernel 6.12.116+deb13-arm64
/dev/loop0 /var/lib/rancher/ambar/volumes/spike2.img   1
22:32:52.201
extent 255 beyond end of bitmap! (32768 >= 32768)
extent 255 beyond end of bitmap! (32768 >= 32768)
Marked additional 0 KB as out-of-sync based on AL.
up rc=0
14:32:52.594 spike2 role:Secondary disk:UpToDate quorum:no open:no lima-dv-1 connection:Connecting lima-dv-3 connection:Connecting
14:32:53.605 spike2 role:Secondary disk:UpToDate quorum:no open:no lima-dv-1 connection:Connecting lima-dv-3 role:Secondary peer-disk:Diskless peer-client:yes
```

Строки `extent 255 beyond end of bitmap!` печатает `drbdadm up` после
принудительной остановки узла с копией [проверить: какая утилита
(`drbdmeta apply-al`) и что они значат для битовой карты на одного пира]. В
образце `stale-diskful-no-quorum.json`, снятом на этом узле до возврата
`dv-1`, битовая карта к `lima-dv-1` уже показывает `out-of-sync: 1044480` и
`percent-in-sync: 0.38`, то есть 1044480 КиБ из 1048508 (99.6 %; 4028 КиБ в
наборе нет) [проверить: откуда эти значения и почему не 1048508].

`drbdsetup status spike2 --verbose` в 22:33:11–12:

```text
== dv-2
spike2 node-id:1 role:Secondary suspended:no force-io-failures:no
  volume:0 minor:1001 disk:UpToDate backing_dev:/dev/ambar/spike2 quorum:no
      open:no blocked:no
  lima-dv-1 node-id:0 connection:Connecting role:Unknown tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Off peer-disk:DUnknown resync-suspended:no
  lima-dv-3 node-id:2 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:Diskless peer-client:yes
        resync-suspended:no
== dv-3
spike2 node-id:2 role:Secondary suspended:no force-io-failures:no
  volume:0 minor:1001 disk:Diskless client:yes backing_dev:none quorum:no open:no
      blocked:no
  lima-dv-1 node-id:0 connection:Connecting role:Unknown tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Off peer-disk:DUnknown resync-suspended:no
  lima-dv-2 node-id:1 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:Consistent resync-suspended:no
```

`dv-2` сам о себе пишет `disk:UpToDate`, хотя его данные старше: текущий UUID
`08136E27BB72DA48` — тот, что на `dv-1` стал битовой картой к `dv-2`.
`dmesg -T` на `dv-2`:

```text
[Thu Nov 12 22:32:52 2026] drbd spike2/0 drbd1001: disk( Attaching -> UpToDate ) [attach]
[Thu Nov 12 22:32:52 2026] drbd spike2/0 drbd1001: attached to current UUID: 08136E27BB72DA48
[Thu Nov 12 22:32:53 2026] drbd spike2/0 drbd1001 lima-dv-3: self 08136E27BB72DA48:0000000000000000:8755DFAC880D4BBA:858046A0E44F38C2 bits:0 flags:0
[Thu Nov 12 22:32:53 2026] drbd spike2/0 drbd1001 lima-dv-3: peer's exposed UUID: B64703DC5853FA4D
[Thu Nov 12 22:32:53 2026] drbd spike2 lima-dv-3: conn( Connecting -> Connected ) peer( Unknown -> Secondary ) [connected]
```

`dv-3`:

```text
[Thu Nov 12 22:32:53 2026] drbd spike2/0 drbd1001 lima-dv-2: my exposed UUID: B64703DC5853FA4D
[Thu Nov 12 22:32:53 2026] drbd spike2/0 drbd1001 lima-dv-2: peer 08136E27BB72DA48:0000000000000000:8755DFAC880D4BBA:858046A0E44F38C2 bits:0 flags:20
[Thu Nov 12 22:32:53 2026] drbd spike2/0 drbd1001 lima-dv-2: Downgrading joining peer's disk as its data is older
[Thu Nov 12 22:32:53 2026] drbd spike2 lima-dv-2: conn( Connecting -> Connected ) peer( Unknown -> Secondary ) [remote]
[Thu Nov 12 22:32:53 2026] drbd spike2/0 drbd1001 lima-dv-2: pdsk( DUnknown -> Outdated ) repl( Off -> Established ) [remote]
[Thu Nov 12 22:32:53 2026] drbd spike2/0 drbd1001 lima-dv-2: pdsk( Outdated -> Consistent ) [peer-state]
```

Образцы: `stale-diskful-no-quorum.json` (`dv-2`), `diskless-stale-peer.json`
(`dv-3`).

Попытки открыть том:

```bash
vm dv-3 "timeout 30 mount /dev/drbd1001 /mnt/spike2; echo mount rc=\$?"
vm dv-2 "timeout 30 mount /dev/drbd1001 /mnt/spike2; echo mount rc=\$?"
vm dv-2 "timeout 30 mount -o ro,noload /dev/drbd1001 /mnt/spike2; echo ro-mount rc=\$?"
stand/spike/ctr.sh dv-3 "timeout 30 drbdadm primary spike2; echo primary rc=\$?"
stand/spike/ctr.sh dv-2 "timeout 30 drbdadm primary spike2; echo primary rc=\$?"
```

```text
== mount dv-3
mount: /mnt/spike2: fsconfig() failed: /dev/drbd1001: Can't open blockdev.
mount rc=32
[Thu Nov 12 22:33:26 2026] drbd spike2: Auto-promote failed: Need access to UpToDate data (-2)
== mount dv-2
mount: /mnt/spike2: fsconfig() failed: /dev/drbd1001: Can't open blockdev.
mount rc=32
[Thu Nov 12 22:33:27 2026] drbd spike2: 1 of 2 nodes visible, need 2 for quorum
[Thu Nov 12 22:33:27 2026] drbd spike2: Auto-promote failed: No quorum (-25)
== ro mount dv-2
ro-mount rc=32
== primary dv-3
spike2: State change failed: (-2) Need access to UpToDate data
primary rc=17
== primary dv-2
spike2: State change failed: (-25) No quorum
additional info from kernel:
1 of 2 nodes visible, need 2 for quorum
primary rc=11
```

`mount` на `dv-3` ждал 5.3 с (22:33:21.191 → 22:33:26.455) и вернул ошибку; на
`dv-2` ошибка сразу. Монтирование не удалось ни на одном узле, сравнивать
журнал и писать нечего. `primary --force` не выполнялся.

#### Вариант A, шаг 3. Возврат новейшей копии `dv-1`

`limactl start --tty=false dv-1` (22:33:51 → 22:34:06), модуль, loop,
`drbdadm up spike2` в 22:34:07.7. Опрос `drbdsetup status spike2` на `dv-1`:

```text
14:34:08.119 spike2 role:Secondary disk:UpToDate open:no lima-dv-2 connection:Connecting lima-dv-3 connection:Connecting
14:34:09.134 spike2 role:Secondary disk:UpToDate open:no lima-dv-2 role:Secondary replication:SyncSource peer-disk:Inconsistent done:4.02 lima-dv-3 role:Secondary peer-disk:Diskless peer-client:yes
...
14:34:19.263 spike2 role:Secondary disk:UpToDate open:no lima-dv-2 role:Secondary replication:SyncSource peer-disk:Inconsistent done:97.94 lima-dv-3 role:Secondary peer-disk:Diskless peer-client:yes
14:34:19.771 spike2 role:Secondary disk:UpToDate open:no lima-dv-2 role:Secondary peer-disk:UpToDate lima-dv-3 role:Secondary peer-disk:Diskless peer-client:yes
```

`dmesg -T` на `dv-1`:

```text
[Thu Nov 12 22:34:08 2026] drbd spike2/0 drbd1001: disk( Attaching -> UpToDate ) quorum( no -> yes ) [attach]
[Thu Nov 12 22:34:08 2026] drbd spike2: Restored quorum from before reboot
[Thu Nov 12 22:34:08 2026] drbd spike2/0 drbd1001 lima-dv-2: self B64703DC5853FA4C:08136E27BB72DA48:8755DFAC880D4BBA:858046A0E44F38C2 bits:4112 flags:2020
[Thu Nov 12 22:34:08 2026] drbd spike2/0 drbd1001 lima-dv-2: peer 08136E27BB72DA48:0000000000000000:8755DFAC880D4BBA:858046A0E44F38C2 bits:261120 flags:2020
[Thu Nov 12 22:34:08 2026] drbd spike2/0 drbd1001 lima-dv-2: uuid_compare()=source-use-bitmap by rule=bitmap-self
[Thu Nov 12 22:34:08 2026] drbd spike2/0 drbd1001 lima-dv-2: pdsk( Outdated -> Inconsistent ) repl( WFBitMapS -> SyncSource ) replication( yes -> no ) [receive-bitmap]
[Thu Nov 12 22:34:08 2026] drbd spike2/0 drbd1001 lima-dv-2: Began resync as SyncSource (will sync 1044480 KB [261120 bits set]).
[Thu Nov 12 22:34:19 2026] drbd spike2/0 drbd1001 lima-dv-2: Resync done (total 11 sec; paused 0 sec; 94952 K/sec)
[Thu Nov 12 22:34:19 2026] drbd spike2/0 drbd1001 lima-dv-2: pdsk( Inconsistent -> UpToDate ) repl( SyncSource -> Established ) [resync-finished]
```

`dv-2`:

```text
[Thu Nov 12 22:34:08 2026] drbd spike2/0 drbd1001 lima-dv-1: self 08136E27BB72DA48:0000000000000000:8755DFAC880D4BBA:858046A0E44F38C2 bits:261120 flags:2020
[Thu Nov 12 22:34:08 2026] drbd spike2/0 drbd1001 lima-dv-1: uuid_compare()=target-use-bitmap by rule=bitmap-peer
[Thu Nov 12 22:34:08 2026] drbd spike2/0 drbd1001: disk( UpToDate -> Outdated ) quorum( no -> yes ) [remote]
[Thu Nov 12 22:34:08 2026] drbd spike2/0 drbd1001: disk( Outdated -> Inconsistent ) [receive-bitmap]
[Thu Nov 12 22:34:08 2026] drbd spike2/0 drbd1001 lima-dv-1: Began resync as SyncTarget (will sync 1044480 KB [261120 bits set]).
[Thu Nov 12 22:34:19 2026] drbd spike2/0 drbd1001 lima-dv-1: Resync done (total 10 sec; paused 0 sec; 104448 K/sec)
[Thu Nov 12 22:34:19 2026] drbd spike2/0 drbd1001: disk( Inconsistent -> UpToDate ) [resync-finished]
```

`dv-3`:

```text
[Thu Nov 12 22:34:08 2026] drbd spike2/0 drbd1001 lima-dv-2: pdsk( Consistent -> Outdated ) [peer-state]
[Thu Nov 12 22:34:08 2026] drbd spike2/0 drbd1001 lima-dv-2: pdsk( Outdated -> Inconsistent ) resync-susp( no -> peer ) replication( yes -> no ) [peer-state]
[Thu Nov 12 22:34:19 2026] drbd spike2/0 drbd1001 lima-dv-2: pdsk( Inconsistent -> UpToDate ) resync-susp( peer -> no ) [peer-state]
```

`grep -i split` по этим строкам пуст, `StandAlone` нет. Досылка на `dv-2` —
261120 бит, 1044480 КиБ из 1048508 КиБ (99.6 %; 4028 КиБ в набор досылки не
вошли), хотя у `dv-1` к `dv-2` было отмечено 4112 бит. Строка `bits:0` в
22:32:53 (`dmesg` `dv-2`) относится к битовой карте `dv-2` к `lima-dv-3`, а не
к `dv-1`. Битовая карта `dv-2` к `dv-1` была почти полной уже до возврата
`dv-1`: образец `stale-diskful-no-quorum.json` (снят на `dv-2` в шаге 2)
показывает для `lima-dv-1` `out-of-sync: 1044480`, `percent-in-sync: 0.38`; при
соединении с `dv-1` в 22:34:08 в `dmesg` — `bits:261120`. В `dmesg` `dv-2` между
22:32:53 и 22:34:08 строк про битовую карту нет [проверить: откуда 1044480 КиБ
в карте к `dv-1` при attach и почему не 1048508 КиБ (разница 4028 КиБ);
261120 = 255 × 1024 — то же число 255, что в `extent 255 beyond end of
bitmap!`; связь не проверена]. В E2 после такой же принудительной остановки
досылка была 16520 КиБ.

Сверка после досылки (`mount` на `dv-3`):

```text
mount rc=0
9349
lines=9349 bad=0
9348
umount rc=0
```

Конечное состояние: `dv-1` и `dv-2` `UpToDate`, `dv-3` `Diskless`, все
`Connected`.

#### Вариант B. Узел без диска тоже перезапущен

Стенд очищен `stand/spike/clean.sh`, `spike2` создан заново, шаг 1 повторён:

```text
22:35:49.903
4084
stop rc=0
22:35:50.094
22:36:15.227
12653
...
22:36:15.761
probe-stopped
acked2=12851
12852
lines=12852 bad=0
umount rc=0
```

Пауза записи: 12 одинаковых выборок подряд с `22:35:50.088 acked=4104`
(~6 с). На `dv-1` `Would lose quorum, but using tiebreaker logic to keep` в
22:35:55, `dv-3` отправил `new current UUID: 68AF08C76065A51B`. ACKED =
12851, на томе 12852, 12852 строки без пропусков. `get-gi` на `dv-1`:
`68AF08C76065A51A:64E7A32F4872D668:4A6BD55C8286E678:F2449244D2A5ECDA:…`.

Отключения и запуск:

```bash
limactl stop --force dv-1        # 22:36:27
limactl stop --force dv-3        # 22:36:30, все три ВМ Stopped
timeout 300 limactl start --tty=false dv-3
limactl start --tty=false dv-2
```

```text
time="2026-11-12T22:36:38+08:00" level=info msg="( 2/5) User session to be ready for SSH (essential)"
start dv-3 rc=124
time="2026-11-12T22:41:57+08:00" level=info msg="READY. Run `limactl shell dv-2` to open the shell."
start dv-2 rc=0
```

Почему `dv-3` не дошёл до `READY` (снято через `ssh -F ~/.lima/dv-3/ssh.config
lima-dv-3` в 22:39:56):

```text
 22:39:56 up 3 min,  1 user,  load average: 0.19, 0.08, 0.02
starting
JOB UNIT                TYPE  STATE
132 cloud-init.target   start waiting
141 cloud-final.service start waiting
124 k3s.service         start running
16  multi-user.target   start waiting
15  graphical.target    start waiting
```

В 22:42:13 на обоих узлах `systemctl is-system-running` → `running`, контейнер
toolbox запущен, модуль не загружен (`lsmod | grep -c ^drbd` → `0`).

DRBD на `dv-3` (узел без диска, после перезагрузки):

```text
22:42:15.346
up rc=0
spike2 node-id:2 role:Secondary suspended:no force-io-failures:no
  volume:0 minor:1001 disk:Diskless client:yes backing_dev:none quorum:no open:no
      blocked:no
  lima-dv-1 node-id:0 connection:Connecting role:Unknown tls:no congested:no
  …
  lima-dv-2 node-id:1 connection:Connecting role:Unknown tls:no congested:no
```

DRBD на `dv-2` (22:42:26.464; снова `extent 255 beyond end of bitmap!` дважды и
`Marked additional 0 KB as out-of-sync based on AL.`), состояние в 22:42:33–34:

```text
== dv-2
  volume:0 minor:1001 disk:UpToDate backing_dev:/dev/ambar/spike2 quorum:no
  lima-dv-1 node-id:0 connection:Connecting role:Unknown tls:no congested:no
  lima-dv-3 node-id:2 connection:Connected role:Secondary tls:no congested:no
    volume:0 replication:Established peer-disk:Diskless peer-client:yes
== dv-3
  volume:0 minor:1001 disk:Diskless client:yes backing_dev:none quorum:no open:no
  lima-dv-1 node-id:0 connection:Connecting role:Unknown tls:no congested:no
  lima-dv-2 node-id:1 connection:Connected role:Secondary tls:no congested:no
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
```

`dmesg -T` на `dv-3`:

```text
[Thu Nov 12 22:42:27 2026] drbd spike2/0 drbd1001 lima-dv-2: my exposed UUID: 0000000000000000
[Thu Nov 12 22:42:27 2026] drbd spike2/0 drbd1001 lima-dv-2: peer 64E7A32F4872D668:0000000000000000:4A6BD55C8286E678:F2449244D2A5ECDA bits:0 flags:20
[Thu Nov 12 22:42:27 2026] drbd spike2/0 drbd1001: Setting exposed data uuid: 64E7A32F4872D668
[Thu Nov 12 22:42:27 2026] drbd spike2 lima-dv-2: conn( Connecting -> Connected ) peer( Unknown -> Secondary ) [remote]
[Thu Nov 12 22:42:27 2026] drbd spike2/0 drbd1001 lima-dv-2: pdsk( DUnknown -> UpToDate ) repl( Off -> Established ) [remote]
```

В отличие от варианта A, узел без диска после перезагрузки не знает о более
новом поколении и принимает UUID устаревшей копии как свой. Образец
`diskless-rebooted-stale-peer.json` (`dv-3`).

Попытки открыть том:

```text
== mount dv-3
mount rc=32
ro-mount rc=32
[Thu Nov 12 22:42:43 2026] drbd spike2: no UpToDate peer with quorum
[Thu Nov 12 22:42:43 2026] drbd spike2: Auto-promote failed: No quorum (-25)
== mount dv-2
mount rc=32
ro-mount rc=32
[Thu Nov 12 22:42:43 2026] drbd spike2: 1 of 2 nodes visible, need 2 for quorum
[Thu Nov 12 22:42:43 2026] drbd spike2: Auto-promote failed: No quorum (-25)
== primary dv-3
spike2: State change failed: (-25) No quorum
additional info from kernel:
no UpToDate peer with quorum
primary rc=11
== primary dv-2
spike2: State change failed: (-25) No quorum
additional info from kernel:
1 of 2 nodes visible, need 2 for quorum
primary rc=11
```

Возврат `dv-1` (22:42:53 → 22:43:08, `drbdadm up` в 22:43:09.5):

```text
[Thu Nov 12 22:43:10 2026] drbd spike2/0 drbd1001: disk( Attaching -> UpToDate ) quorum( no -> yes ) [attach]
[Thu Nov 12 22:43:10 2026] drbd spike2: Restored quorum from before reboot
[Thu Nov 12 22:43:10 2026] drbd spike2/0 drbd1001 lima-dv-2: self 68AF08C76065A51A:64E7A32F4872D668:4A6BD55C8286E678:F2449244D2A5ECDA bits:4115 flags:2020
[Thu Nov 12 22:43:10 2026] drbd spike2/0 drbd1001 lima-dv-2: peer 64E7A32F4872D668:0000000000000000:4A6BD55C8286E678:F2449244D2A5ECDA bits:261120 flags:2020
[Thu Nov 12 22:43:10 2026] drbd spike2/0 drbd1001 lima-dv-2: uuid_compare()=source-use-bitmap by rule=bitmap-self
[Thu Nov 12 22:43:10 2026] drbd spike2/0 drbd1001 lima-dv-2: Began resync as SyncSource (will sync 1044480 KB [261120 bits set]).
[Thu Nov 12 22:43:10 2026] drbd spike2/0 drbd1001 lima-dv-3: peer's exposed UUID: 64E7A32F4872D668
[Thu Nov 12 22:43:21 2026] drbd spike2/0 drbd1001 lima-dv-2: Resync done (total 10 sec; paused 0 sec; 104448 K/sec)
```

На `dv-3` при соединении с `dv-1`: `my exposed UUID: 64E7A32F4872D668` →
`Setting exposed data uuid: 68AF08C76065A51A`, `pdsk( UpToDate -> Outdated )`
и `Outdated -> Inconsistent` для `dv-2`. `grep -i split` пуст. Итоговая сверка
(узел, на котором она выполнена, и время в выводе не записаны; в варианте A
сверка шла на `dv-3`):

```text
12851
mount rc=0
12852
lines=12852 bad=0
umount rc=0
```

#### `quorum-minimum-redundancy`

```bash
stand/spike/ctr.sh dv-1 'drbdsetup help resource-options 2>&1 | grep -i -E -A3 "redundancy|--quorum"'
```

```text
    [--quorum={off|majority|all}|(1 ... 32)] [--on-no-quorum={io-error|suspend-io}]
    [--quorum-minimum-redundancy={off|majority|all}|(1 ... 32)]
    [--on-suspended-primary-outdated={disconnect|force-secondary}]
    [--drbd8-api-compatibility={yes|no}]
```

`drbdsetup show spike --show-defaults` (в опыте C3) печатает
`quorum-minimum-redundancy	off; # default`. Шаг 5 брифа не выполнялся: ни
в варианте A, ни в варианте B устаревшую копию не удалось повысить.

| Наблюдение C1 (по одному прогону на вариант) | Вариант A (`dv-3` не перезапускался) | Вариант B (`dv-3` перезапущен) |
|---|---|---|
| Кворум у {устаревшая копия + узел без диска} | нет на обоих (`quorum:no`) | нет на обоих (`quorum:no`) |
| Как узел без диска видит устаревшую копию | `Consistent` (`Downgrading joining peer's disk as its data is older`) | `UpToDate` (`my exposed UUID: 0000000000000000`, принял UUID копии) |
| Как устаревшая копия видит себя | `disk:UpToDate` | `disk:UpToDate` |
| `mount` rw / `mount -o ro,noload` / `drbdadm primary` на узле без диска | `mount` rw и `primary`: отказ `Need access to UpToDate data (-2)`; `mount -o ro,noload` не выполнялся | все три: отказ, `No quorum (-25)`, `no UpToDate peer with quorum` |
| То же на устаревшей копии | `mount` rw, `mount -o ro,noload`, `primary`: отказ, `No quorum (-25)`, `1 of 2 nodes visible, need 2 for quorum` (причина отказа `ro`-монтирования не записана) | отказ, те же сообщения |
| Запись | не пробовали: том не открылся; `primary --force` не выполнялся | не пробовали: том не открылся; `primary --force` не выполнялся |
| Возврат `dv-1` | `Restored quorum from before reboot`, `SyncSource`, досылка 1044480 КиБ из 1048508 (99.6 %) за 10–11 с, split-brain нет | то же, досылка 1044480 КиБ за 10 с |
| На томе / ACKED | 9349 / 9348 | 12852 / 12851 |
| Пауза записи при принудительной остановке `dv-2` | 11.3 с между выборками | ~6 с (12 выборок) |

#### Что из этого следует для агента

- Состоянию диска узла о самом себе верить нельзя: устаревшая копия после
  перезапуска пишет `disk:UpToDate` (`stale-diskful-no-quorum.json`, строка
  `disk:UpToDate backing_dev:/dev/ambar/spike2 quorum:no` на `dv-2`). В
  обоих прогонах `quorum:no` совпало с отказом `mount` и `drbdadm primary`.
  Обратное (`quorum:yes` на устаревшей копии означает, что открывать можно) не
  проверялось.
- Узел без диска после перезагрузки показывает устаревшую копию как
  `peer-disk:UpToDate` (`diskless-rebooted-stale-peer.json`). Выбор узла для
  пода по `peer-disk-state` на узле без диска может указать на устаревшую
  копию; в этом прогоне повышение всё равно отклонено по кворуму
  (`no UpToDate peer with quorum`).
- В двух прогонах (A и B, один стенд) пара {устаревшая копия, узел без диска}
  не получила кворум (`1 of 2 nodes visible, need 2 for quorum`). В E2 узел без
  диска вместе с оставшейся копией кворум сохранил (`Would lose quorum, but
  using tiebreaker logic to keep`); в C1 тот же узел без диска не дал кворума
  заново запущенной устаревшей копии [проверить: tiebreaker сохраняет уже
  имевшийся кворум, но не даёт его заново запущенному узлу — предположение].
  Ошибка `mount` в этом случае — `Can't open blockdev` (`rc=32`), причина
  видна только в `dmesg` (`Auto-promote failed: …`) или в выводе `drbdadm
  primary` (`rc=11` для `No quorum`, `rc=17` для `Need access to UpToDate
  data`).
- Запись на устаревшую копию не пробовали, `drbdadm primary --force` не
  выполнялся: данных о том, что произошло бы при принудительном повышении, нет.
- Узел с новейшей копией, загруженный в одиночку, получает кворум при attach
  (`Restored quorum from before reboot`, `quorum( no -> yes ) [attach]` до
  соединения с кем-либо). Можно ли открыть том на таком узле, пока остальные
  выключены, не проверялось [проверить].
- После принудительной остановки узла с копией досылка при его возврате в двух
  прогонах (A и B) была 261120 бит, 1044480 КиБ из 1048508 (99.6 %), за 10–11 с
  (`Resync done (total 11 sec …)` на `dv-1` в варианте A, `total 10 sec` на
  `dv-2` в варианте A и на `dv-1` в варианте B), а не по небольшой битовой карте,
  как в E2 (16520 КиБ) [проверить: причина]. Это два прогона на одном стенде; для
  томов другого размера время досылки не измерялось.
- Одиночная ВМ стенда с k3s-server не дошла до `READY` за 300 с, пока две другие
  были выключены (`k3s.service start running`, `multi-user.target start
  waiting`) [проверить: причина — ожидание кворума etcd]. Доступность toolbox и
  модуля DRBD на ВМ, пока она была одна, не проверялась [проверить]; после
  запуска второго узла toolbox был запущен, модуль не загружен (`lsmod | grep -c
  ^drbd` → `0`).

### Опыт C2. Изолированный `Primary`: пиры не продвинулись; `adjust` до размонтирования

Ресурс `spike` из трёх копий (`res.sh spike 1000 7000 - io-error`, затем
`new-current-uuid --clear-bitmap`, `mkfs.ext4`), том смонтирован на `dv-1`,
писатель и `watch.sh` на `dv-1`. Изоляция — те же правила `nft`, что в шаге 6
(файл `/var/tmp/nft-spike.nft`, таблица `inet spike`). Два прогона, по одному
на случай.

#### Прогон 1. Пиры не продвинулись, том на `dv-1` не размонтирован

```bash
vm dv-1 "date +%T.%3N; nft -f /var/tmp/nft-spike.nft; date +%T.%3N"
sleep 20
vm dv-1 "date +%T.%3N; cat /var/tmp/acked; cat /var/tmp/probe.log; ps -eo pid,stat,args | grep '[p]robe\.sh' || echo probe-gone; findmnt -o TARGET,OPTIONS /mnt/spike"
```

```text
22:44:45.687
22:44:45.691
22:45:05.852
5072
sync: error syncing '/mnt/spike/log': Input/output error
write 5073 failed
probe-gone
TARGET     OPTIONS
/mnt/spike rw,relatime
spike role:Primary
  disk:UpToDate quorum:no open:yes
  lima-dv-2 connection:Connecting
  lima-dv-3 connection:Connecting
```

`watch.log`: `22:44:45.698 acked=5072 probe=alive:S` → `22:44:56.427
acked=5072 probe=gone`. Том больше нигде не монтировался.

Снятие правил в 22:45:13.36 и опрос `drbdsetup status spike` на трёх узлах
75 с (показаны смены состояния):

```text
22:45:13 dv-1: spike role:Primary disk:UpToDate quorum:no open:yes lima-dv-2 connection:Connecting lima-dv-3 connection:Connecting
22:45:15 dv-1: spike role:Primary disk:UpToDate open:yes lima-dv-2 role:Secondary replication:SyncSource peer-disk:Inconsistent lima-dv-3 role:Secondary peer-disk:UpToDate
22:45:16 dv-1: spike role:Primary disk:UpToDate open:yes lima-dv-2 role:Secondary peer-disk:UpToDate lima-dv-3 role:Secondary peer-disk:UpToDate
22:45:13 dv-2: spike role:Secondary disk:UpToDate open:no lima-dv-1 connection:Connecting lima-dv-3 role:Secondary peer-disk:UpToDate
22:45:16 dv-2: spike role:Secondary disk:UpToDate open:no lima-dv-1 role:Primary peer-disk:UpToDate lima-dv-3 role:Secondary peer-disk:UpToDate
22:45:14 dv-3: spike role:Secondary disk:UpToDate open:no lima-dv-1 connection:Connecting lima-dv-2 role:Secondary peer-disk:UpToDate
22:45:16 dv-3: spike role:Secondary disk:UpToDate open:no lima-dv-1 role:Primary peer-disk:UpToDate lima-dv-2 role:Secondary peer-disk:UpToDate
...
22:46:27 dv-1: spike role:Primary disk:UpToDate open:yes lima-dv-2 role:Secondary peer-disk:UpToDate lima-dv-3 role:Secondary peer-disk:UpToDate
```

`dmesg -T` на `dv-1` (без строк `Preparing`/`Committing`):

```text
[Thu Nov 12 22:45:15 2026] drbd spike/0 drbd1000 lima-dv-3: self 8DD686239630F7AC:0000000000000000:0000000000000000:0000000000000000 bits:2 flags:2520
[Thu Nov 12 22:45:15 2026] drbd spike/0 drbd1000 lima-dv-3: peer 8DD686239630F7AC:0000000000000000:0000000000000000:0000000000000000 bits:0 flags:120
[Thu Nov 12 22:45:15 2026] drbd spike/0 drbd1000 lima-dv-3: uuid_compare()=no-sync by rule=reconnected
[Thu Nov 12 22:45:15 2026] drbd spike/0 drbd1000 lima-dv-3: strategy = source-use-bitmap to reconcile equal-UUID peers holding out-of-sync bits
[Thu Nov 12 22:45:15 2026] drbd spike lima-dv-3: conn( Connecting -> Connected ) peer( Unknown -> Secondary ) [connected]
[Thu Nov 12 22:45:15 2026] drbd spike/0 drbd1000: quorum( no -> yes ) [connected]
[Thu Nov 12 22:45:15 2026] drbd spike/0 drbd1000: new current UUID: F5A9EE4B634046E1 weak: FFFFFFFFFFFFFFFA
[Thu Nov 12 22:45:15 2026] drbd spike/0 drbd1000 lima-dv-3: Began resync as SyncSource (will sync 8 KB [2 bits set]).
[Thu Nov 12 22:45:15 2026] drbd spike/0 drbd1000 lima-dv-2: self F5A9EE4B634046E1:8DD686239630F7AC:0000000000000000:0000000000000000 bits:2 flags:2120
[Thu Nov 12 22:45:15 2026] drbd spike/0 drbd1000 lima-dv-2: peer 8DD686239630F7AC:0000000000000000:0000000000000000:0000000000000000 bits:0 flags:120
[Thu Nov 12 22:45:15 2026] drbd spike/0 drbd1000 lima-dv-2: uuid_compare()=source-use-bitmap by rule=bitmap-self
[Thu Nov 12 22:45:15 2026] drbd spike/0 drbd1000 lima-dv-2: Began resync as SyncSource (will sync 8 KB [2 bits set]).
[Thu Nov 12 22:45:16 2026] drbd spike/0 drbd1000 lima-dv-3: Resync done (total 1 sec; paused 0 sec; 8 K/sec)
[Thu Nov 12 22:45:16 2026] drbd spike/0 drbd1000 lima-dv-2: Resync done (total 1 sec; paused 0 sec; 8 K/sec)
```

`dv-3` (выдержка):

```text
[Thu Nov 12 22:45:16 2026] drbd spike/0 drbd1000 lima-dv-1: uuid_compare()=no-sync by rule=reconnected
[Thu Nov 12 22:45:16 2026] drbd spike/0 drbd1000 lima-dv-1: strategy = target-use-bitmap to reconcile equal-UUID peers holding out-of-sync bits
[Thu Nov 12 22:45:16 2026] drbd spike lima-dv-1: conn( Connecting -> Connected ) peer( Unknown -> Primary ) [remote]
[Thu Nov 12 22:45:16 2026] drbd spike/0 drbd1000 lima-dv-1: not adopting new current UUID F5A9EE4B634046E1 on Outdated disk
[Thu Nov 12 22:45:16 2026] drbd spike/0 drbd1000: Multiple UpToDate peers have different current UUIDs
[Thu Nov 12 22:45:16 2026] drbd spike/0 drbd1000: lima-dv-2: 8DD686239630F7AC lima-dv-1: F5A9EE4B634046E1
[Thu Nov 12 22:45:16 2026] drbd spike/0 drbd1000 lima-dv-1: Began resync as SyncTarget (will sync 8 KB [2 bits set]).
[Thu Nov 12 22:45:16 2026] drbd spike/0 drbd1000: disk( Inconsistent -> UpToDate ) [resync-finished]
```

`dv-2`: `disk( UpToDate -> Outdated ) [far-away]`, `uuid_compare()=target-use-bitmap
by rule=bitmap-peer`, `Began resync as SyncTarget (will sync 8 KB [2 bits set])`,
`disk( Inconsistent -> UpToDate ) [resync-finished]`. `StandAlone` и
`Split-Brain` нет.

Состояние тома на `dv-1` после переподключения (22:46:51, том не
перемонтировался):

```bash
vm dv-1 "cat /var/tmp/acked; tail -1 /mnt/spike/log; …; echo x >> /mnt/spike/log; sync /mnt/spike/log; touch /mnt/spike/t2; findmnt -o TARGET,OPTIONS /mnt/spike; dmesg -T | grep -i ext4 | tail -3"
```

```text
5072
5073
tail: error reading '/mnt/spike/log': Input/output error
append rc=1
awk: cannot open "/mnt/spike/log" (Input/output error)
fsync rc=1
touch: cannot touch '/mnt/spike/t2': Input/output error
TARGET     OPTIONS
/mnt/spike rw,relatime
[Thu Nov 12 22:46:51 2026] EXT4-fs error (device drbd1000): ext4_journal_check_start:84: comm tail: Detected aborted journal
[Thu Nov 12 22:46:51 2026] EXT4-fs (drbd1000): Remounting filesystem read-only
```

В этом выводе `findmnt` показывает `rw,relatime`, хотя ядро уже пишет
`Remounting filesystem read-only`. `tail -1` первым выводом показал `5073` из
кэша страниц, затем ошибку чтения. DRBD-устройство после переподключения пишется (три копии
`UpToDate`, `quorum` есть), но ext4 на `dv-1` после `EIO` остаётся с
прерванным журналом. Перемонтирование:

```text
umount rc=0
spike role:Secondary
  disk:UpToDate open:no
  lima-dv-2 role:Secondary
    peer-disk:UpToDate
  lima-dv-3 role:Secondary
    peer-disk:UpToDate
mount rc=0
5072
lines=5072 bad=0
write+fsync rc=0
[Thu Nov 12 22:46:52 2026] EXT4-fs (drbd1000): warning: mounting fs with errors, running e2fsck is recommended
[Thu Nov 12 22:46:52 2026] EXT4-fs (drbd1000): recovery complete
umount rc=0
```

ACKED = 5072, на томе 5072 строки без пропусков; 5073 на томе нет. Запись
`99999` с `fsync` после перемонтирования прошла (`write+fsync rc=0`).

#### Прогон 2. Пиры продвинулись, `adjust` до и после размонтирования

Перед прогоном: `e2fsck -fy /dev/drbd1000` на `dv-1` (`rc=0`), `mount`,
журнал писателя удалён, писатель и `watch.sh` запущены заново.

```text
22:47:12.379
22:47:12.383
22:47:32.551
2997
sync: error syncing '/mnt/spike/log': Input/output error
write 2998 failed
probe-gone
22:47:12.740 acked=2997 probe=alive:S
22:47:23.540 acked=2997 probe=gone
```

Запись на `dv-2` (пиры продвигаются):

```bash
vm dv-2 "mount /dev/drbd1000 /mnt/spike; tail -1 /mnt/spike/log; for i in 1 2 3 4 5; do echo dv2-\$i >> /mnt/spike/log && sync /mnt/spike/log && echo ok-\$i; done; wc -l < /mnt/spike/log"
```

```text
22:47:41.583
mount rc=0
2997
ok-1
...
ok-5
3002
```

Снятие правил в 22:47:41.86:

```text
22:47:41 dv-1: spike role:Primary disk:UpToDate quorum:no open:yes lima-dv-2 connection:Connecting lima-dv-3 connection:Connecting
22:47:43 dv-1: spike role:Primary disk:UpToDate quorum:no open:yes lima-dv-2 connection:Connecting lima-dv-3 connection:StandAlone
22:47:45 dv-1: spike role:Primary disk:UpToDate quorum:no open:yes lima-dv-2 connection:StandAlone lima-dv-3 connection:StandAlone
22:47:45 dv-2: spike role:Primary disk:UpToDate open:yes lima-dv-1 connection:StandAlone lima-dv-3 role:Secondary peer-disk:UpToDate
22:47:42 dv-3: spike role:Secondary disk:UpToDate open:no lima-dv-1 connection:StandAlone lima-dv-2 role:Primary peer-disk:UpToDate
```

```text
[Thu Nov 12 22:47:43 2026] drbd spike/0 drbd1000 lima-dv-3: I shall become SyncTarget, but I am primary!
[Thu Nov 12 22:47:43 2026] drbd spike lima-dv-3: conn( Disconnecting -> StandAlone ) [disconnected]
[Thu Nov 12 22:47:45 2026] drbd spike/0 drbd1000 lima-dv-2: I shall become SyncTarget, but I am primary!
[Thu Nov 12 22:47:45 2026] drbd spike lima-dv-2: conn( Disconnecting -> StandAlone ) [disconnected]
```

Это та же картина, что в E1. `adjust` на трёх узлах при `Primary` на `dv-1`
(том смонтирован: `open:yes` в строках опроса ниже; вывод `findmnt` в этом
прогоне не записан):

```bash
for n in dv-1 dv-2 dv-3; do stand/spike/ctr.sh $n "date +%T.%3N; timeout 30 drbdadm adjust spike; echo adjust rc=\$?"; done
```

```text
14:48:22.447
adjust rc=0
14:48:22.866
adjust rc=0
14:48:23.294
adjust rc=0
22:48:23 dv-1: spike role:Primary disk:UpToDate quorum:no open:yes lima-dv-2 connection:StandAlone lima-dv-3 connection:Connecting
22:48:24 dv-1: spike role:Primary disk:UpToDate quorum:no open:yes lima-dv-2 connection:StandAlone lima-dv-3 connection:StandAlone
...
22:48:47 dv-1: spike role:Primary disk:UpToDate quorum:no open:yes lima-dv-2 connection:StandAlone lima-dv-3 connection:StandAlone
```

`dmesg -T` на `dv-1`:

```text
[Thu Nov 12 22:48:22 2026] drbd spike lima-dv-2: conn( StandAlone -> Unconnected ) [connect]
[Thu Nov 12 22:48:22 2026] drbd spike lima-dv-3: conn( StandAlone -> Unconnected ) [connect]
[Thu Nov 12 22:48:23 2026] drbd spike/0 drbd1000 lima-dv-2: self F5A9EE4B634046E0:0000000000000000:8DD686239630F7AC:0000000000000000 bits:3 flags:2520
[Thu Nov 12 22:48:23 2026] drbd spike/0 drbd1000 lima-dv-2: peer 12DD86498ABEABEF:F5A9EE4B634046E0:0000000000000000:0000000000000000 bits:25 flags:2120
[Thu Nov 12 22:48:23 2026] drbd spike/0 drbd1000 lima-dv-2: uuid_compare()=target-use-bitmap by rule=bitmap-peer
[Thu Nov 12 22:48:23 2026] drbd spike/0 drbd1000 lima-dv-2: I shall become SyncTarget, but I am primary!
[Thu Nov 12 22:48:23 2026] drbd spike: Aborting cluster-wide state change 343269666 (16ms) rv = -27
[Thu Nov 12 22:48:23 2026] drbd spike lima-dv-2: conn( Disconnecting -> StandAlone ) [disconnected]
[Thu Nov 12 22:48:23 2026] drbd spike/0 drbd1000 lima-dv-3: I shall become SyncTarget, but I am primary!
[Thu Nov 12 22:48:23 2026] drbd spike lima-dv-3: conn( Disconnecting -> StandAlone ) [disconnected]
```

`dv-2`:

```text
[Thu Nov 12 22:48:23 2026] drbd spike lima-dv-1: conn( StandAlone -> Unconnected ) [connect]
[Thu Nov 12 22:48:23 2026] drbd spike: State change failed: Multiple primaries not allowed by config (-1)
[Thu Nov 12 22:48:23 2026] drbd spike/0 drbd1000 lima-dv-1: uuid_compare()=source-use-bitmap by rule=bitmap-self
[Thu Nov 12 22:48:23 2026] drbd spike lima-dv-1: conn( Disconnecting -> StandAlone ) [disconnected]
```

`adjust` при смонтированном томе: связи переходят в `Unconnected` →
`Connecting`, соединение устанавливается, ядро отказывает с тем же
сообщением и через ~1 с связи снова `StandAlone`. Ошибки `adjust` не
возвращает (`rc=0`).

Размонтирование на `dv-1` и `adjust` на трёх узлах:

```text
22:48:59.537
umount rc=0
not-mounted
Secondary
spike role:Secondary
  disk:UpToDate quorum:no open:no
  lima-dv-2 connection:StandAlone
  lima-dv-3 connection:StandAlone
14:49:00.430
adjust rc=0
14:49:00.881
adjust rc=0
14:49:01.539
adjust rc=0
22:49:01 dv-1: spike role:Secondary disk:UpToDate open:no lima-dv-2 role:Primary peer-disk:UpToDate lima-dv-3 connection:Connecting
22:49:02 dv-1: spike role:Secondary disk:UpToDate open:no lima-dv-2 role:Primary peer-disk:UpToDate lima-dv-3 role:Secondary peer-disk:UpToDate
22:49:02 dv-2: spike role:Primary disk:UpToDate open:yes lima-dv-1 role:Secondary peer-disk:UpToDate lima-dv-3 role:Secondary peer-disk:UpToDate
22:49:02 dv-3: spike role:Secondary disk:UpToDate open:no lima-dv-1 role:Secondary peer-disk:UpToDate lima-dv-2 role:Primary peer-disk:UpToDate
```

`drbdadm secondary` не понадобился: `umount` перевёл узел в `Secondary`
(`role( Primary -> Secondary ) [umount:23450 auto-demote]`). `dmesg -T` на
`dv-1`:

```text
[Thu Nov 12 22:49:01 2026] drbd spike/0 drbd1000 lima-dv-2: uuid_compare()=target-use-bitmap by rule=bitmap-peer
[Thu Nov 12 22:49:01 2026] drbd spike lima-dv-2: conn( Connecting -> Connected ) peer( Unknown -> Primary ) [connected]
[Thu Nov 12 22:49:01 2026] drbd spike/0 drbd1000: disk( UpToDate -> Outdated ) quorum( no -> yes ) [connected]
[Thu Nov 12 22:49:01 2026] drbd spike/0 drbd1000: disk( Outdated -> Inconsistent ) [receive-bitmap]
[Thu Nov 12 22:49:01 2026] drbd spike/0 drbd1000 lima-dv-2: Began resync as SyncTarget (will sync 100 KB [25 bits set]).
[Thu Nov 12 22:49:01 2026] drbd spike/0 drbd1000: disk( Inconsistent -> UpToDate ) [resync-finished]
[Thu Nov 12 22:49:02 2026] drbd spike/0 drbd1000 lima-dv-3: uuid_compare()=no-sync by rule=reconnected
[Thu Nov 12 22:49:02 2026] drbd spike lima-dv-3: conn( Connecting -> Connected ) peer( Unknown -> Secondary ) [connected]
```

Сверка на `dv-2` (ACKED на `dv-1` = 2997):

```text
3002
2995 2996 2997 dv2-1 dv2-2 dv2-3 dv2-4 dv2-5
first2997 bad=0
umount rc=0
```

| Наблюдение C2 (по одному прогону) | Прогон 1: пиры не продвинулись | Прогон 2: пиры продвинулись |
|---|---|---|
| Связи после снятия правил при `Primary` на `dv-1` | восстановились сами за ~2 с (22:45:13.4 → 22:45:15–16), `dv-1` — `SyncSource`, 8 КиБ | `StandAlone` через 2–4 с, `I shall become SyncTarget, but I am primary!` |
| Новый текущий UUID | `dv-1` при соединении: `new current UUID: F5A9EE4B634046E1` | — |
| `adjust` на трёх узлах при смонтированном томе | не выполнялся | `rc=0`, попытка соединения, тот же отказ, снова `StandAlone` через ~1 с |
| `adjust` на трёх узлах после `umount` на `dv-1` | не понадобился | соединение и досылка 100 КиБ на `dv-1` за ~2 с |
| ext4 на `dv-1` после переподключения | `Detected aborted journal`, `EIO` на чтение и запись до `umount` + `mount`; после — `mounting fs with errors`, запись проходит | том размонтирован до `adjust` |
| На томе / ACKED | 5072 / 5072 | 2997 подтверждённых + 5 записей `dv-2` / 2997 |

#### Что из этого следует для агента

- После `EIO` на изолированном узле ext4 на этом узле в прогоне 1 не
  вернулась в рабочее состояние сама, хотя DRBD переподключился и все копии
  были `UpToDate` (22:46:51: `Detected aborted journal`, `Remounting filesystem
  read-only`). Момент аборта журнала неизвестен: первые строки ext4 в выводе
  `dmesg | grep -i ext4 | tail -3` относятся к 22:46:51, когда `tail` наткнулся
  на уже прерванный журнал. В прогоне 1 `EIO` на запись ext4 вернула ещё до
  возврата сети (`write 5073 failed` в журнале писателя). После `umount` и повторного
  `mount` ядро пишет `mounting fs with errors, running e2fsck is recommended`,
  запись проходит (один прогон).
- Если пиры не продвинулись, связи изолированного `Primary` восстанавливаются
  без вмешательства (прогон 1, `uuid_compare()=no-sync by rule=reconnected` и
  `source-use-bitmap`). `StandAlone` наблюдался в прогоне 2, где `dv-2`
  записал данные до снятия правил (`target-use-bitmap` + `I shall become
  SyncTarget, but I am primary!`), и в E1 с тем же сообщением; в прогоне 1
  `StandAlone` не было. Других условий появления `StandAlone` не проверяли.
- `drbdadm adjust` при смонтированном томе и роли `Primary` возвращает `rc=0`,
  но связи снова уходят в `StandAlone` (прогон 2, 22:48:23). Код возврата
  `adjust` не говорит, восстановилась ли связь; проверять надо
  `connection-state` после него. Порядок, который сработал: `umount` (узел
  сам уходит в `Secondary`), затем `adjust` на трёх узлах.

### Опыт C3. Настоящий split-brain в DRBD 9.3.4

Цель: получить настоящий split-brain на ресурсе с выключенным кворумом и
сравнить его признаки со `StandAlone` из E1. Один прогон.

Ресурс `spike` создан `res.sh spike 1000 7000 - io-error`, затем в `.res` на
трёх узлах строка `options` заменена на `options { quorum off; auto-promote
yes; }`:

```bash
vm <узел> "sed -i 's/quorum majority; on-no-quorum io-error;/quorum off;/' /var/lib/rancher/ambar/res/spike.res"
stand/spike/ctr.sh <узел> "drbdadm dump spike >/dev/null && echo dump ok; drbdadm adjust spike; echo adjust rc=\$?; drbdsetup show spike --show-defaults | grep -E '^\s+quorum|on-no-quorum'"
```

```text
    options { quorum off; auto-promote yes; }
dump ok
adjust rc=0
        quorum          	off; # default
        on-no-quorum    	suspend-io; # default
        quorum-minimum-redundancy	off; # default
```

Вывод одинаков на трёх узлах. Затем `new-current-uuid --clear-bitmap`
(14:50:11 UTC), три `UpToDate`, `mkfs.ext4`, `mount` на `dv-1`, писатель и
`watch.sh` на `dv-1`.

Изоляция `dv-1` и запись на обеих сторонах:

```text
22:50:46.119
22:50:46.129
22:51:06.318
6380
  26962 S    bash /usr/local/bin/probe.sh /mnt/spike /var/tmp/acked
     21 22:50:46.465 acked=896 probe=alive:S
spike role:Primary
  disk:UpToDate open:yes
  lima-dv-2 connection:Connecting
  lima-dv-3 connection:Connecting
[Thu Nov 12 22:50:56 2026] drbd spike lima-dv-2: PingAck did not arrive in time.
[Thu Nov 12 22:50:56 2026] drbd spike lima-dv-3: PingAck did not arrive in time.
[Thu Nov 12 22:50:56 2026] drbd spike/0 drbd1000: new current UUID: 3BB88FDA87BA31F1 weak: FFFFFFFFFFFFFFFE
```

С `quorum off` изолированный `dv-1` после паузы (21 одинаковая выборка с
22:50:46.465, ~10.5 с) продолжает подтверждать записи. Затем:

```bash
vm dv-2 "mount /dev/drbd1000 /mnt/spike; tail -1 /mnt/spike/log; wc -l < /mnt/spike/log; for i in 1 2 3 4 5; do echo dv2-\$i >> /mnt/spike/log && sync /mnt/spike/log && echo ok-\$i; done; echo dv2-file > /mnt/spike/dv2-only && sync /mnt/spike/dv2-only && echo dv2-only ok; wc -l < /mnt/spike/log"
vm dv-1 "pkill -f '^bash /usr/local/bin/probe\.sh'; sleep 1; echo acked=\$(cat /var/tmp/acked); tail -1 /mnt/spike/log; wc -l < /mnt/spike/log; echo dv1-file > /mnt/spike/dv1-only && sync /mnt/spike/dv1-only && echo dv1-only ok"
vm dv-1 "umount /mnt/spike"; vm dv-2 "umount /mnt/spike"
```

```text
22:51:17.013
mount rc=0
896
896
ok-1
...
ok-5
dv2-only ok
901
22:51:17.609
acked=15875
15876
15876
dv1-only ok
22:51:19.186
umount rc=0
22:51:19.743
umount rc=0
```

После `umount` обе стороны `Secondary`. На `dv-2` том открылся с журналом до
896 — это последнее подтверждение до изоляции; `dv-1` после изоляции
подтвердил ещё 897–15875.

Перед снятием правил на трёх узлах запущен `timeout 45 drbdsetup events2
--timestamps spike`. Снятие правил в 22:51:35.45, опрос:

```text
22:51:35 dv-1: spike role:Secondary disk:UpToDate open:no lima-dv-2 connection:Connecting lima-dv-3 connection:Connecting
22:51:37 dv-1: spike role:Secondary disk:UpToDate open:no lima-dv-2 connection:Connecting lima-dv-3 connection:StandAlone
22:51:38 dv-1: spike role:Secondary disk:UpToDate open:no lima-dv-2 connection:StandAlone lima-dv-3 connection:StandAlone
22:51:37 dv-2: spike role:Secondary disk:UpToDate open:no lima-dv-1 connection:StandAlone lima-dv-3 role:Secondary peer-disk:UpToDate
22:51:38 dv-3: spike role:Secondary disk:UpToDate open:no lima-dv-1 connection:StandAlone lima-dv-2 role:Secondary peer-disk:UpToDate
```

`dmesg -T` на `dv-1` (без потоков и `Feature flags`):

```text
[Thu Nov 12 22:51:37 2026] drbd spike lima-dv-3: Handshake to peer 2 successful: Agreed network protocol version 125
[Thu Nov 12 22:51:37 2026] drbd spike: Preparing cluster-wide state change 3474031710: 0->2 role( Secondary ) conn( Connected )
[Thu Nov 12 22:51:37 2026] drbd spike/0 drbd1000 lima-dv-3: drbd_sync_handshake:
[Thu Nov 12 22:51:37 2026] drbd spike/0 drbd1000 lima-dv-3: self 3BB88FDA87BA31F0:56D2FD426ECBD8AC:0000000000000000:0000000000000000 bits:4126 flags:2120
[Thu Nov 12 22:51:37 2026] drbd spike/0 drbd1000 lima-dv-3: peer 1A2099DE875E62C8:56D2FD426ECBD8AC:56D2FD426ECBD8AC:0000000000000000 bits:35 flags:2120
[Thu Nov 12 22:51:37 2026] drbd spike/0 drbd1000 lima-dv-3: uuid_compare()=split-brain-auto-recover by rule=bitmap-both
[Thu Nov 12 22:51:37 2026] drbd spike/0 drbd1000 lima-dv-3: Split-Brain detected but unresolved, dropping connection!
[Thu Nov 12 22:51:37 2026] drbd spike: Aborting cluster-wide state change 3474031710 (16ms) rv = -27
[Thu Nov 12 22:51:37 2026] drbd spike lima-dv-3: conn( Connecting -> Disconnecting ) [connect-failed]
[Thu Nov 12 22:51:37 2026] drbd spike lima-dv-3: Connection closed
[Thu Nov 12 22:51:37 2026] drbd spike lima-dv-3: conn( Disconnecting -> StandAlone ) [disconnected]
[Thu Nov 12 22:51:37 2026] drbd spike lima-dv-2: Handshake to peer 1 successful: Agreed network protocol version 125
[Thu Nov 12 22:51:37 2026] drbd spike: Preparing cluster-wide state change 709506331: 0->1 role( Secondary ) conn( Connected )
[Thu Nov 12 22:51:37 2026] drbd spike/0 drbd1000 lima-dv-2: drbd_sync_handshake:
[Thu Nov 12 22:51:37 2026] drbd spike/0 drbd1000 lima-dv-2: self 3BB88FDA87BA31F0:56D2FD426ECBD8AC:0000000000000000:0000000000000000 bits:4126 flags:2120
[Thu Nov 12 22:51:37 2026] drbd spike/0 drbd1000 lima-dv-2: peer 1A2099DE875E62C8:56D2FD426ECBD8AC:56D2FD426ECBD8AC:0000000000000000 bits:35 flags:2120
[Thu Nov 12 22:51:37 2026] drbd spike/0 drbd1000 lima-dv-2: uuid_compare()=split-brain-auto-recover by rule=bitmap-both
[Thu Nov 12 22:51:37 2026] drbd spike/0 drbd1000 lima-dv-2: Split-Brain detected but unresolved, dropping connection!
[Thu Nov 12 22:51:37 2026] drbd spike: Aborting cluster-wide state change 709506331 (28ms) rv = -27
[Thu Nov 12 22:51:37 2026] drbd spike lima-dv-2: conn( Connecting -> Disconnecting ) [connect-failed]
[Thu Nov 12 22:51:37 2026] drbd spike lima-dv-2: Connection closed
[Thu Nov 12 22:51:37 2026] drbd spike lima-dv-2: conn( Disconnecting -> StandAlone ) [disconnected]
```

`dv-2`:

```text
[Thu Nov 12 22:51:38 2026] drbd spike/0 drbd1000 lima-dv-1: self 1A2099DE875E62C8:56D2FD426ECBD8AC:56D2FD426ECBD8AC:0000000000000000 bits:35 flags:2120
[Thu Nov 12 22:51:38 2026] drbd spike/0 drbd1000 lima-dv-1: peer 3BB88FDA87BA31F0:56D2FD426ECBD8AC:0000000000000000:0000000000000000 bits:4126 flags:2120
[Thu Nov 12 22:51:38 2026] drbd spike/0 drbd1000 lima-dv-1: uuid_compare()=split-brain-auto-recover by rule=bitmap-both
[Thu Nov 12 22:51:38 2026] drbd spike/0 drbd1000 lima-dv-1: Split-Brain detected but unresolved, dropping connection!
[Thu Nov 12 22:51:38 2026] drbd spike lima-dv-1: Aborting remote state change 709506331
[Thu Nov 12 22:51:38 2026] drbd spike lima-dv-1: conn( Connecting -> Disconnecting ) [receive-disconnect]
[Thu Nov 12 22:51:38 2026] drbd spike lima-dv-1: Connection closed
[Thu Nov 12 22:51:38 2026] drbd spike lima-dv-1: conn( Disconnecting -> StandAlone ) [disconnected]
```

`dmesg` `dv-3` в отчёте не приведён. О связи `dv-3` с `dv-1` видно из двух
выводов: `drbdsetup status` на `dv-3` в 22:51:38 показывает `lima-dv-1
connection:StandAlone`, а в `dmesg` `dv-1` (22:51:37) для `lima-dv-3`
`peer 1A2099DE875E62C8:…` — тот же текущий UUID, что `self` у `dv-2`, и то же
`Split-Brain detected but unresolved`. Сообщение ядра на стороне `dv-3` не
записано.
`dmesg -T | grep -i split` на `dv-1` и `dv-2` находит кроме этих строк ещё
`drbd spike2 lima-dv-3: Cluster is now split` (22:44:03 на `dv-1`, 22:44:06 на
`dv-2`) и `drbd spike lima-dv-3: Cluster is now split` (22:49:44, 22:49:47) —
это моменты `stand/spike/clean.sh` (`drbdsetup down` на `dv-3`), а не
split-brain.

Сравнение с E1 по ядру:

| Признак | E1 (`StandAlone` из-за роли) | C3 (split-brain) |
|---|---|---|
| `uuid_compare()` на отказавшей стороне | `target-use-bitmap by rule=bitmap-peer` | `split-brain-auto-recover by rule=bitmap-both` |
| Строка отказа | `I shall become SyncTarget, but I am primary!` | `Split-Brain detected but unresolved, dropping connection!` |
| UUID при рукопожатии | у одной стороны текущий UUID другой стороны стоит в истории (`peer 199B…:3FED…`, `self 3FED…`) | обе стороны имеют разные текущие UUID с общим UUID битовой карты `56D2FD426ECBD8AC` |
| Роль узла | `Primary`, том смонтирован | обе стороны `Secondary` |

`events2 --timestamps` на `dv-1` с 14:51:31 UTC (запускался на трёх узлах; вывод
`dv-2` и `dv-3` в отчёте не приведён):

```text
2026-11-12T14:51:37.226272+00:00 change path name:spike peer-node-id:2 conn-name:lima-dv-3 local:ipv4:192.168.104.1:7000 peer:ipv4:192.168.104.4:7000 established:yes
2026-11-12T14:51:37.245370+00:00 change peer-device name:spike peer-node-id:2 conn-name:lima-dv-3 volume:0
2026-11-12T14:51:37.245722+00:00 change connection name:spike peer-node-id:2 conn-name:lima-dv-3 connection:Disconnecting
2026-11-12T14:51:37.261134+00:00 change path name:spike peer-node-id:2 conn-name:lima-dv-3 local:ipv4:192.168.104.1:7000 peer:ipv4:192.168.104.4:7000 established:no
2026-11-12T14:51:37.263143+00:00 change connection name:spike peer-node-id:2 conn-name:lima-dv-3 connection:StandAlone
2026-11-12T14:51:37.800284+00:00 change path name:spike peer-node-id:1 conn-name:lima-dv-2 local:ipv4:192.168.104.1:7000 peer:ipv4:192.168.104.3:7000 established:yes
2026-11-12T14:51:37.824457+00:00 change peer-device name:spike peer-node-id:1 conn-name:lima-dv-2 volume:0
2026-11-12T14:51:37.835099+00:00 change connection name:spike peer-node-id:1 conn-name:lima-dv-2 connection:Disconnecting
2026-11-12T14:51:37.857132+00:00 change path name:spike peer-node-id:1 conn-name:lima-dv-2 local:ipv4:192.168.104.1:7000 peer:ipv4:192.168.104.3:7000 established:no
2026-11-12T14:51:37.857869+00:00 change connection name:spike peer-node-id:1 conn-name:lima-dv-2 connection:StandAlone
```

В `events2` нет поля или события со словом split-brain: только
`established:yes` → `Disconnecting` → `StandAlone`. Захват `events2` в E1 не
снимался, поэтому побайтовое сравнение событий с E1 не выполнено; по
переходам `connection:` последовательность совпадает с `dmesg` E1
(`Connecting -> Disconnecting -> StandAlone`). `events2 --now --statistics`
на `dv-1` после split-brain:

```text
exists resource name:spike role:Secondary suspended:no force-io-failures:no write-ordering:flush may_promote:yes promotion_score:10101
exists connection name:spike peer-node-id:2 conn-name:lima-dv-3 connection:StandAlone role:Unknown congested:no ap-in-flight:0 rs-in-flight:0
exists connection name:spike peer-node-id:1 conn-name:lima-dv-2 connection:StandAlone role:Unknown congested:no ap-in-flight:0 rs-in-flight:0
exists device name:spike volume:0 minor:1000 backing_dev:/dev/ambar/spike disk:UpToDate client:no open:no quorum:yes size:1048476 read:1005 written:1336772 al-writes:5 bm-writes:20 upper-pending:0 lower-pending:0 al-suspended:no blocked:no
exists peer-device name:spike peer-node-id:2 conn-name:lima-dv-3 volume:0 replication:Off peer-disk:DUnknown peer-client:no resync-suspended:no received:0 sent:0 out-of-sync:16504 pending:0 unacked:0
exists path name:spike peer-node-id:2 conn-name:lima-dv-3 local:ipv4:192.168.104.1:7000 peer:ipv4:192.168.104.4:7000 established:no
exists peer-device name:spike peer-node-id:1 conn-name:lima-dv-2 volume:0 replication:Off peer-disk:DUnknown peer-client:no resync-suspended:no received:0 sent:0 out-of-sync:16504 pending:0 unacked:0
exists path name:spike peer-node-id:1 conn-name:lima-dv-2 local:ipv4:192.168.104.1:7000 peer:ipv4:192.168.104.3:7000 established:no
exists -
```

`drbdsetup status spike --verbose --statistics` и `drbdadm get-gi spike/0`
(22:52:39–42):

```text
== dv-1
spike node-id:0 role:Secondary suspended:no force-io-failures:no
    write-ordering:flush
  volume:0 minor:1000 disk:UpToDate backing_dev:/dev/ambar/spike quorum:yes
      open:no
      size:1048476 read:1005 written:1336772 al-writes:5 bm-writes:20 upper-pending:0
      lower-pending:0 al-suspended:no blocked:no
  lima-dv-2 node-id:1 connection:StandAlone role:Unknown tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Off peer-disk:DUnknown resync-suspended:no
        received:0 sent:0 out-of-sync:16504 pending:0 unacked:0
  lima-dv-3 node-id:2 connection:StandAlone role:Unknown tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Off peer-disk:DUnknown resync-suspended:no
        received:0 sent:0 out-of-sync:16504 pending:0 unacked:0

3BB88FDA87BA31F0:56D2FD426ECBD8AC:0000000000000000:0000000000000000:1:1:0:0:0:0:0:0:0:0:0:1:1
3BB88FDA87BA31F0:56D2FD426ECBD8AC:0000000000000000:0000000000000000:1:1:0:0:0:0:0:0:0:0:0:1:1
== dv-2
spike node-id:1 role:Secondary suspended:no force-io-failures:no
  volume:0 minor:1000 disk:UpToDate backing_dev:/dev/ambar/spike quorum:yes
  lima-dv-1 node-id:0 connection:StandAlone role:Unknown tls:no congested:no
    volume:0 replication:Off peer-disk:DUnknown resync-suspended:no
        received:0 sent:0 out-of-sync:140 pending:0 unacked:0
  lima-dv-3 node-id:2 connection:Connected role:Secondary tls:no congested:no
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no

1A2099DE875E62C8:56D2FD426ECBD8AC:56D2FD426ECBD8AC:0000000000000000:1:1:0:0:0:0:0:0:0:0:0:1:1
1A2099DE875E62C8:0000000000000000:56D2FD426ECBD8AC:0000000000000000:1:1:0:0:0:0:0:1:0:0:0:1:0
```

Вывод `dv-2` сокращён (строки статистики устройства опущены); `dv-3`
совпадает с `dv-2` по состояниям. `quorum:yes` при `quorum off` — в этом захвате
поле `yes`; для других состояний при `quorum off` оно не снималось.

Образцы: `split-brain-victim.json` (`dv-1`), `split-brain-survivor.json`
(`dv-2`). Сравнение по полям с `standalone-primary.json` (E1, `dv-1`)
скриптом `stand/spike/compare-fixtures.py` (раскладывает JSON в плоский
словарь «путь → значение»; вызов `python3 stand/spike/compare-fixtures.py
stand/spike/samples/split-brain-victim.json stand/spike/samples/standalone-primary.json`):

Вывод ниже снят ранним ad-hoc вариантом скрипта (подписи `E1`/`SB`); повторный запуск сохранённого скрипта даёт те же различия.

```text
=== split-brain-victim.json vs standalone-primary.json
keys only in split-brain-victim.json []
keys only in E1 []
  connections[lima-dv-2].peer_devices[0].out-of-sync: E1=8 SB=16504
  connections[lima-dv-2].peer_devices[0].percent-in-sync: E1=100.0 SB=98.43
  connections[lima-dv-3].peer_devices[0].out-of-sync: E1=12 SB=16504
  connections[lima-dv-3].peer_devices[0].percent-in-sync: E1=100.0 SB=98.43
  devices[0].bm-writes: E1=18 SB=20
  devices[0].open: E1=True SB=False
  devices[0].quorum: E1=False SB=True
  devices[0].written: E1=1104748 SB=1336772
  role: E1='Primary' SB='Secondary'
```

Набор ключей одинаков. Отличаются роль, `open`, `quorum` (следствие
`quorum off` и `Secondary` в C3) и счётчики. Поля, которое отмечает
split-brain, в `drbdsetup status --json` нет.

#### Разрешение

Жертва — `dv-1` (его записи 897–15875 и файл `dv1-only` отбрасываются; на томе у `dv-1` было 15876 строк, последняя не подтверждена писателем).

```bash
stand/spike/ctr.sh dv-1 "drbdadm disconnect spike; echo disconnect rc=\$?; drbdadm connect --discard-my-data spike; echo connect rc=\$?"
```

```text
14:53:03.825
disconnect rc=0
connect rc=0
[Thu Nov 12 22:53:03 2026] drbd spike: State change failed: Can not disconnect a StandAlone device (-9)
[Thu Nov 12 22:53:03 2026] drbd spike lima-dv-2: Failed: conn( StandAlone -> Disconnecting ) [disconnect]
[Thu Nov 12 22:53:03 2026] drbd spike: State change failed: Can not disconnect a StandAlone device (-9)
[Thu Nov 12 22:53:03 2026] drbd spike lima-dv-3: Failed: conn( StandAlone -> Disconnecting ) [disconnect]
[Thu Nov 12 22:53:03 2026] drbd spike lima-dv-2: conn( StandAlone -> Unconnected ) [connect]
[Thu Nov 12 22:53:03 2026] drbd spike lima-dv-2: conn( Unconnected -> Connecting ) [connecting]
[Thu Nov 12 22:53:03 2026] drbd spike lima-dv-3: conn( StandAlone -> Unconnected ) [connect]
[Thu Nov 12 22:53:03 2026] drbd spike lima-dv-3: conn( Unconnected -> Connecting ) [connecting]
```

Через ~20 с после этого `dv-1` был в `Connecting` к обоим, а `dv-2` и `dv-3`
держали `lima-dv-1 connection:StandAlone`, в их `dmesg` новых строк не было;
вывод этой проверки не записан. По этому наблюдению одной команды на жертве
недостаточно. Затем на выживших:

```bash
for n in dv-2 dv-3; do stand/spike/ctr.sh $n "drbdadm connect spike; echo connect rc=\$?"; done
```

```text
14:53:33.958
spike: Failure: (125) Device has a net-config (use disconnect first)
Command 'drbdsetup connect spike 2' terminated with exit code 10
connect rc=10
14:53:34.369
spike: Failure: (125) Device has a net-config (use disconnect first)
Command 'drbdsetup connect spike 1' terminated with exit code 10
connect rc=10
22:53:34 dv-1: spike role:Secondary disk:UpToDate open:no lima-dv-2 role:Secondary peer-disk:UpToDate lima-dv-3 connection:Connecting
22:53:35 dv-1: spike role:Secondary disk:UpToDate open:no lima-dv-2 role:Secondary peer-disk:UpToDate lima-dv-3 role:Secondary peer-disk:UpToDate
22:53:34 dv-2: spike role:Secondary disk:UpToDate open:no lima-dv-1 role:Secondary peer-disk:UpToDate lima-dv-3 role:Secondary peer-disk:UpToDate
22:53:35 dv-3: spike role:Secondary disk:UpToDate open:no lima-dv-1 role:Secondary peer-disk:UpToDate lima-dv-2 role:Secondary peer-disk:UpToDate
```

Код 10 вернули `drbdsetup connect spike 2` на `dv-2` (пир `dv-3`) и
`drbdsetup connect spike 1` на `dv-3` (пир `dv-2`), то есть команды для уже
`Connected` пар. Вывод `drbdadm connect` для пары с `lima-dv-1` и `dmesg`
выживших не записаны (переход `StandAlone -> Unconnected` на них не
зафиксирован); по `drbdsetup status` в 22:53:34–35 связь с `lima-dv-1`
поднялась. `dmesg -T` на `dv-1`:

```text
[Thu Nov 12 22:53:34 2026] drbd spike/0 drbd1000 lima-dv-2: self 3BB88FDA87BA31F0:56D2FD426ECBD8AC:0000000000000000:0000000000000000 bits:4126 flags:2121
[Thu Nov 12 22:53:34 2026] drbd spike/0 drbd1000 lima-dv-2: peer 1A2099DE875E62C8:56D2FD426ECBD8AC:56D2FD426ECBD8AC:0000000000000000 bits:35 flags:2120
[Thu Nov 12 22:53:34 2026] drbd spike/0 drbd1000 lima-dv-2: uuid_compare()=split-brain-auto-recover by rule=bitmap-both
[Thu Nov 12 22:53:34 2026] drbd spike/0 drbd1000 lima-dv-2: Split-Brain detected, manually solved. Sync from peer node
[Thu Nov 12 22:53:34 2026] drbd spike lima-dv-2: conn( Connecting -> Connected ) peer( Unknown -> Secondary ) [connected]
[Thu Nov 12 22:53:34 2026] drbd spike/0 drbd1000: disk( UpToDate -> Outdated ) [connected]
[Thu Nov 12 22:53:34 2026] drbd spike/0 drbd1000: disk( Outdated -> Inconsistent ) [receive-bitmap]
[Thu Nov 12 22:53:34 2026] drbd spike/0 drbd1000 lima-dv-2: Began resync as SyncTarget (will sync 16512 KB [4128 bits set]).
[Thu Nov 12 22:53:34 2026] drbd spike/0 drbd1000 lima-dv-2: Resync done (total 1 sec; paused 0 sec; 16512 K/sec)
[Thu Nov 12 22:53:34 2026] drbd spike/0 drbd1000: disk( Inconsistent -> UpToDate ) [resync-finished]
[Thu Nov 12 22:53:34 2026] drbd spike/0 drbd1000 lima-dv-3: uuid_compare()=no-sync by rule=reconnected
[Thu Nov 12 22:53:34 2026] drbd spike lima-dv-3: conn( Connecting -> Connected ) peer( Unknown -> Secondary ) [connected]
```

`dv-2`: `Split-Brain detected, manually solved. Sync from this node`,
`Began resync as SyncSource (will sync 16512 KB [4128 bits set])`. В
`flags:2121` у жертвы младший бит отличается от `flags:2120` до команды
[проверить: что это флаг `discard-my-data`]. Проверка на `dv-1`:

```text
15875
mount rc=0
dv2-only
log
lost+found
901
dv2-4 dv2-5
umount rc=0
```

На `dv-1` данные `dv-2`: 901 строка, последняя `dv2-5`, файла `dv1-only`
нет. Записи 897–15875, которые писатель на `dv-1` получил как
подтверждённые (ACKED на `dv-1` = 15875), отброшены: потеря произошла. Она
следствие запуска с `quorum off`. В проектной конфигурации (`quorum majority`)
split-brain в этом опыте не воспроизводился и не проверялся; единственное
свидетельство о `quorum majority` — опыт E1 (изолированный `Primary` с
`quorum:no` получает `EIO`, подтверждённые данные на месте).

| Наблюдение C3 (один прогон) | Значение |
|---|---|
| Запись на обеих сторонах при `quorum off` | `dv-1` подтвердил 897–15875 после изоляции, `dv-2` — `dv2-1`…`dv2-5` и `dv2-only` |
| Сообщение ядра | `uuid_compare()=split-brain-auto-recover by rule=bitmap-both`, затем `Split-Brain detected but unresolved, dropping connection!` |
| Состояние после | на жертве обе связи `StandAlone`, на выживших — связь с жертвой `StandAlone`; все `Secondary`, `disk:UpToDate` |
| Потеря данных | записи 897–15875 с `dv-1` отброшены; следствие `quorum off`, при `quorum majority` не проверялось |
| Отличие от E1 в `status --json` / `events2` | в `status --json` поля нет, набор ключей совпадает; в захвате `events2` C3 отличающего события нет, `events2` для E1 не снимался |
| Время от снятия правил до `StandAlone` | ~2 с (22:51:35.45 → 22:51:37–38) |
| Разрешение | на жертве `disconnect` (ядро: `Can not disconnect a StandAlone device`, `rc=0`) + `connect --discard-my-data`; на двух других узлах `connect` (`rc=10` из-за уже соединённого пира, связь с жертвой поднялась); досылка 16512 КиБ за 1 с |
| Сообщение после разрешения | жертва: `Split-Brain detected, manually solved. Sync from peer node`; выживший: `… Sync from this node` |

#### Что из этого следует для агента

- Потеря записей 897–15875 в этом опыте — следствие запуска с `quorum off`. В
  конфигурации `quorum majority` split-brain не получен и не проверялся;
  свидетельство о ней только E1 (`EIO` на изолированном `Primary`, данные
  целы).
- В `drbdsetup status --json` нет поля, которым split-brain отличался бы от
  `StandAlone` из E1 (набор ключей совпадает). В захвате `events2` опыта C3
  только `connection:Disconnecting` → `StandAlone`; `events2` для E1 не
  снимался, поэтому сравнить нельзя. Записанное различие — текст сообщения ядра:
  `Split-Brain detected but unresolved, dropping connection!` против `I shall
  become SyncTarget, but I am primary!` (и `uuid_compare()=…by rule=bitmap-both`
  против `…by rule=bitmap-peer`).
- Гипотеза, не проверенная как признак: у сторон при split-brain разные
  текущие UUID при общем UUID битовой карты (`3BB88FDA87BA31F0:56D2FD426ECBD8AC`
  и `1A2099DE875E62C8:56D2FD426ECBD8AC`), в E1 текущий UUID одной стороны стоял
  в истории другой. `drbdadm get-gi` снят только после C3, для E1 не снимался
  [проверить: годится ли сравнение UUID как признак split-brain].
- Поиск по `dmesg` словом `split` без привязки к формулировке даёт ложные
  совпадения: `Cluster is now split` печатается при `drbdsetup down` на
  пире (22:44:03, 22:49:44 на `dv-1`).
- Разрешение потребовало команд на всех узлах со `StandAlone`-связью к жертве:
  после `connect --discard-my-data` на `dv-1` пиры оставались `StandAlone`
  (проверка через ~20 с, вывод не записан). Коды возврата `drbdadm` здесь не
  показывают результат: `disconnect` дал `rc=0` при отказе ядра, `connect` дал
  `rc=10` для уже соединённых пар; результат виден в `connection-state`.
- Связь `dv-3` с жертвой тоже оказалась `StandAlone` (`drbdsetup status` на
  `dv-3`, 22:51:38), хотя `dv-3` записей не принимал; сообщение ядра на
  стороне `dv-3` не записано.
- Поле `quorum` в статусе при `quorum off` было `yes` в одном захвате
  (`split-brain-victim.json`, `dv-1`); для других состояний при `quorum off`
  поле не снималось.

### Очистка стенда после опытов C1–C3

В `stand/spike/clean.sh` добавлено удаление `/var/tmp/nft-spike.nft` (файл
правил изоляции из C2 и C3).

```bash
stand/spike/clean.sh
stand/check.sh
kc get nodes --no-headers
for n in dv-1 dv-2 dv-3; do vm $n "cat /sys/module/drbd/version; ps -eo pid,comm,args | grep -E '[p]robe\.sh|[w]atch\.sh' || echo no probe/watch; echo loops=\$(losetup -l -n | wc -l); ls /dev/ambar; nft list tables | grep spike || echo no-spike-table; findmnt /mnt/spike /mnt/spike2 || echo no-mounts"; stand/spike/ctr.sh $n "drbdsetup status"; done
```

```text
clean rc=0
OK: stand is up
lima-dv-1   Ready   control-plane,etcd   95m   v1.37.2+k3s1
lima-dv-2   Ready   control-plane,etcd   94m   v1.37.2+k3s1
lima-dv-3   Ready   control-plane,etcd   93m   v1.37.2+k3s1
== dv-1
9.3.4
no probe/watch
loops=0
ls: cannot access '/dev/ambar': No such file or directory
no-spike-table
no-mounts
# No currently configured DRBD found.
```

`dv-2` и `dv-3` дали тот же вывод. `/var/tmp` на трёх узлах после повторного
`clean.sh`: `cloud-init` и каталог `systemd-private-…`.

## Опыты перед агентом (P1–P6)

Дата: 2026-11-13. Стенд тот же: три ВМ Lima. Версии (ядро `6.12.116+deb13-arm64`, DRBD 9.3.4, drbd-utils 9.35.0) записаны выше в этом документе (`drbdadm --version` в поде `toolbox`); в опытах P1–P6 версии заново не снимались. Опыты выполняет `stand/spike/pre-agent.sh <p1|…|p6>`: перед опытом и после него скрипт останавливает ресурсы `p1`–`p6`, отсоединяет loop-устройства и удаляет файлы. Каждый опыт запущен один раз. Перед первым опытом состояние стенда проверялось (`stand/check.sh`, `drbdsetup status` на трёх узлах, `losetup -a`); вывод этой проверки не сохранён. Состояние после опытов записано в конце раздела.

| Опыт | Вопрос | Результат |
|---|---|---|
| P1 | Работает ли `losetup --find --show --direct-io=on` из привилегированного контейнера и видит ли узел то же устройство | да: в контейнере `/dev/loop0`, `DIO` = `1`, `losetup -j` в контейнере и на узле печатает одну и ту же строку; символьная ссылка `/dev/ambar/p1`, созданная в контейнере, видна на узле |
| P2 | Принимает ли `create-md` флаг `--max-peers=7` и каков `size` устройства для файла 1 ГиБ | флаг принят в форме `--max-peers=7`, `rc=0`, битовая карта 224 КБ; `size` = 1048316 КиБ, как по формуле |
| P3 | Что печатает `drbdsetup status --json` без ресурсов и для несуществующего имени | без ресурсов `rc=0`, stdout `[`, `]`, пустая строка (пустой JSON-массив); для `nosuch` `rc=10`, stdout тот же пустой массив, stderr `nosuch: No such resource` |
| P4 | Что делает `drbdadm adjust` на ресурсе, который не поднят | поднимает: на каждом из трёх узлов `rc=0`, ресурс настроен в ядре и соединён, у узла без диска `disk:Diskless client:yes` |
| P5 | Что нужно, чтобы добавить сторону без диска к работающему ресурсу с писателем, и прерывается ли запись | `adjust` на двух узлах и `up` на новом были достаточны: все три связи `Connected`, строк `quorum(` на узлах с копией при добавлении стороны (08:49:21–08:50:11) нет, `quorum:yes`; в выборках через ~0.5 с номер рос каждый раз, состояние писателя `S`/`R`, ошибок нет (остановка короче интервала не видна); необходимость каждого шага не проверялась; последний подтверждённый номер 4206, последний на томе 4207 |
| P6 | Что делает `drbdadm up` для файла без метаданных | `rc=1`, `No valid meta data found`; ресурс остаётся наполовину настроенным (`disk:Diskless`, соединения `StandAlone`, `quorum:no`), `drbdadm adjust` даёт ту же ошибку |

### Отклонения от брифа и правки скрипта

| Место | Что сказано | Что наблюдалось и что сделано |
|---|---|---|
| P3, ожидание в брифе | без ресурсов `drbdsetup status --json` «не печатает JSON-массив» | печатает пустой массив `[`, `]` и пустую строку (`od -c`: `[ \n ] \n \n`), `rc=0` |
| P3, `nosuch` | «ненулевой код» | `rc=10`; stdout — тот же пустой массив, сообщение об ошибке идёт в stderr (проверено повтором с `2>/dev/null` и `2>&1 >/dev/null`) |
| `pre-agent.sh`, `cleanup` | не удалял каталог `/mnt/pa` | добавлено `rmdir /mnt/pa 2>/dev/null \|\| true` после `umount`; правка внесена после опытов P1–P6 и проверена запуском `pre-agent.sh clean` |

Остальной текст скрипта выполнен без правок.

### P1. loop-устройства из контейнера

Вопрос: хватает ли привилегированного контейнера, чтобы агент подключал файл тома как loop-устройство с прямым вводом-выводом.

```bash
stand/spike/pre-agent.sh p1
```

```text
### P1: loop devices from inside the container
$ [dv-1] truncate -s 64M /var/lib/rancher/ambar/volumes/p1.img && losetup --find --show --direct-io=on /var/lib/rancher/ambar/volumes/p1.img
/dev/loop0
rc=0
$ [dv-1] losetup -l -O NAME,BACK-FILE,DIO | grep p1.img
/dev/loop0 /var/lib/rancher/ambar/volumes/p1.img   1
rc=0
$ [dv-1] losetup -j /var/lib/rancher/ambar/volumes/p1.img
/dev/loop0: [65025]:262650 (/var/lib/rancher/ambar/volumes/p1.img)
rc=0
$ [dv-1 host] losetup -j /var/lib/rancher/ambar/volumes/p1.img
/dev/loop0: [65025]:262650 (/var/lib/rancher/ambar/volumes/p1.img)
rc=0
$ [dv-1] mkdir -p /dev/ambar && ln -sfn $(losetup -j /var/lib/rancher/ambar/volumes/p1.img | cut -d: -f1) /dev/ambar/p1 && ls -l /dev/ambar/p1
lrwxrwxrwx 1 root root 10 Nov 13 00:44 /dev/ambar/p1 -> /dev/loop0
rc=0
$ [dv-1 host] ls -l /dev/ambar/p1
lrwxrwxrwx 1 root root 10 Nov 13 08:44 /dev/ambar/p1 -> /dev/loop0
rc=0
$ [dv-1] losetup -d $(losetup -j /var/lib/rancher/ambar/volumes/p1.img | cut -d: -f1)
rc=0
```

Итог: в контейнере `losetup --find --show --direct-io=on` вернул `/dev/loop0`, `DIO` = `1`; `losetup -j` в контейнере и на узле называет `/dev/loop0` с одним и тем же `[65025]:262650`; ссылка `/dev/ambar/p1`, созданная в контейнере, видна на узле. Один прогон, один узел (`dv-1`).

### P2. `create-md --max-peers=7` и размер устройства

Вопрос: принимает ли drbd-utils 9.35.0 флаг `--max-peers` у `drbdadm create-md` и какой `size` получает устройство для файла 1 ГиБ. Ресурс `p2`: три стороны с диском (`node-id` 0, 1, 2), порт 7002, minor 1002.

```bash
stand/spike/pre-agent.sh p2
```

```text
### P2: create-md --max-peers=7, device size for a 1 GiB file
$ [dv-1] drbdadm create-md --force --max-peers=7 p2
initializing bitmap (224 KB) to all zero
initializing activity log
Writing meta data...
New drbd meta data block successfully created.
rc=0
$ [dv-1] drbdadm up p2
rc=0
$ [dv-2] drbdadm create-md --force --max-peers=7 p2
initializing bitmap (224 KB) to all zero
initializing activity log
Writing meta data...
New drbd meta data block successfully created.
rc=0
$ [dv-2] drbdadm up p2
rc=0
$ [dv-3] drbdadm create-md --force --max-peers=7 p2
initializing bitmap (224 KB) to all zero
initializing activity log
Writing meta data...
New drbd meta data block successfully created.
rc=0
$ [dv-3] drbdadm up p2
rc=0
$ [dv-1] drbdsetup status p2 --json | grep -E '"size"|"disk-state"|"connection-state"'
      "disk-state": "Inconsistent",
      "size": 1048316,
      "connection-state": "Connected", 
      "connection-state": "Connected", 
rc=0
expected "size" by the formula (ceil(sectors/2^18)*8*7+72 sectors of metadata): 1048316
$ [dv-1] drbdadm new-current-uuid --clear-bitmap p2/0
rc=0
$ [dv-1] drbdsetup status p2 --json | grep -E '"disk-state"|"peer-disk-state"'
      "disk-state": "UpToDate",
          "peer-disk-state": "UpToDate",
          "peer-disk-state": "UpToDate",
rc=0
```

Дополнительно (чтение справки, без изменений на стенде):

```bash
drbdadm create-md --help | grep -iE "max-peers|peer|force"
drbdmeta --help | grep -i peers
```

```text
  --max-peers=...
  --peer-max-bio-size=...
  --force[=...]
  --diskful-peers=...
  --peers=...
  --peers-outdated[=...]
  restore-md file [--max-peers {val}] [--peers {val}]
  convert-dump file [--peers {val}] [--bitmap-slots {val}] [--bitmap-block-size {val}]
  create-md [--effective-size {val}] [--diskful-peers {val}] [--peer-max-bio-size {val}] [--al-stripes {val}] [--al-stripe-size-kB {val}] [--bitmap-block-size {val}] [--peers {val}] [--bitmap-slots {val}] {max_peers}
```

Размеры метаданных из выводов P2, P3, P4 и P5 (файл 1 ГиБ, один прогон каждого):

| Опыт | Команда `create-md` | Стороны с диском в файле ресурса | Битовая карта | `size`, КиБ |
|---|---|---|---|---|
| P2 | `--force --max-peers=7` | 3 | 224 КБ | 1048316 (`drbdsetup status --json` на `dv-1`) |
| P5 | `--force --max-peers=7` | 2 (третья сторона без диска добавлена позже) | 224 КБ | 1048316 на `dv-3` после подключения (`dmesg`: `size = 1024 MB (1048316 KB)`); на `dv-1` и `dv-2` до добавления `dv-3` `size` не снимался |
| P3 | `--force` | 3 | 64 КБ | не снимался |
| P4 | `--force` | 2 (третья сторона без диска) | 32 КБ | не снимался |

Расчёт по формуле из брифа: 1 ГиБ = 2097152 секторов; `ceil(2097152 / 2^18) = 8`; 8 × 8 × 7 = 448 секторов (224 КиБ) битовой карты плюс 72 сектора (36 КиБ) = 520 секторов = 260 КиБ; 1048576 − 260 = 1048316. Совпадает с наблюдением. Размер проверен для одного размера файла (1 ГиБ) и одного значения `--max-peers`. `[проверить: формула для других размеров файла и значений --max-peers]`

Итог: форма `--max-peers=7` принята, `rc=0`, `size` = 1048316 КиБ. С `--max-peers=7` `create-md` создал 224 КиБ битовой карты и при трёх сторонах с диском в файле ресурса (P2), и при двух (P5, третья сторона добавлена позже). В P3 и P4 без флага размер карты равен 32 КБ на каждую из остальных сторон с диском (без локальной): две остальные стороны с диском дали 64 КБ (P3), одна — 32 КБ (P4). Узел без диска `dv-3` в P5 (`node-id` 2) после подключения сообщил размер устройства 1048316 КиБ, как в P2. `size` на `dv-1` и `dv-2` до добавления `dv-3` не записан, поэтому изменился ли он при добавлении, по записи не видно. После `new-current-uuid --clear-bitmap` все три копии стали `UpToDate` за ≤ 3 с (одна выборка через `sleep 3`).

### P3. `drbdsetup status --json` без ресурсов

Вопрос: что печатает команда, когда в ядре нет ресурсов, и для имени, которого нет.

```bash
stand/spike/pre-agent.sh p3
```

```text
### P3: drbdsetup status --json without resources
$ [dv-1] drbdsetup status --json
[
]

rc=0
$ [dv-1] drbdsetup status
# No currently configured DRBD found.
rc=0
$ [dv-1] drbdsetup status nosuch --json
nosuch: No such resource
[
]

rc=10

### P3: the same with one configured resource
$ [dv-1] drbdadm create-md --force p3 && drbdadm up p3
initializing bitmap (64 KB) to all zero
initializing activity log
Writing meta data...
New drbd meta data block successfully created.
rc=0
$ [dv-1] drbdsetup status --json | head -c 300
[
{
  "name": "p3",
  "node-id": 0,
  "role": "Secondary",
  "suspended": false,
  "suspended-user": false,
  "suspended-no-data": false,
  "suspended-fencing": false,
  "suspended-quorum": false,
  "force-io-failures": false,
  "write-ordering": "flush",
  "devices": [
    {
      "volume": 0,
    rc=0
$ [dv-1] drbdsetup status nosuch --json
[
]

nosuch: No such resource
rc=10
```

Поток вывода проверен отдельно (ресурсов на стенде нет):

```bash
drbdsetup status --json 2>/dev/null | od -c; drbdsetup status nosuch --json 2>/dev/null | od -c
drbdsetup status nosuch --json 2>&1 >/dev/null; drbdsetup status --json 2>&1 >/dev/null
```

```text
A
0000000   [  \n   ]  \n  \n
0000005
rc=0
B
0000000   [  \n   ]  \n  \n
0000005
rc=10
C
nosuch: No such resource
rc=10
D
rc=0
```

Итог: без ресурсов `drbdsetup status --json` возвращает `rc=0` и печатает в stdout пустой JSON-массив (`[`, `]`, пустая строка), не пустой вывод. Для несуществующего имени `rc=10`, stdout тот же пустой массив, stderr `nosuch: No such resource`. С одним настроенным ресурсом `p3` вывод начинается с `[` и `{` с полями `name`, `node-id`, `role`, `suspended`…, `rc=0`; для `nosuch` при настроенном `p3` те же `rc=10` и пустой массив.

### P4. `drbdadm adjust` на ресурсе, который не поднят

Вопрос: поднимает ли `adjust` ресурс, которого в ядре нет, или отказывает. Ресурс `p4`: стороны `dv-1` (диск), `dv-2` (диск), `dv-3` (`disk none`); метаданные созданы на `dv-1` и `dv-2` без `--max-peers`; `up` не вызывался ни на одном узле.

```bash
stand/spike/pre-agent.sh p4
```

```text
### P4: drbdadm adjust on a resource that is not up
$ [dv-1] drbdadm create-md --force p4
initializing bitmap (32 KB) to all zero
initializing activity log
Writing meta data...
New drbd meta data block successfully created.
rc=0
$ [dv-2] drbdadm create-md --force p4
initializing bitmap (32 KB) to all zero
initializing activity log
Writing meta data...
New drbd meta data block successfully created.
rc=0

### P4: node with a copy, resource never configured: adjust instead of up
$ [dv-1] drbdadm adjust p4
rc=0
$ [dv-1] drbdsetup status p4
p4 role:Secondary
  disk:Inconsistent quorum:no open:no
  lima-dv-2 connection:Connecting
  lima-dv-3 connection:Connecting

rc=0

### P4: second node with a copy: adjust
$ [dv-2] drbdadm adjust p4
rc=0

### P4: node without a disk: adjust
$ [dv-3] drbdadm adjust p4
rc=0
$ [dv-1] drbdsetup status p4 --verbose
p4 node-id:0 role:Secondary suspended:no force-io-failures:no
  volume:0 minor:1004 disk:Inconsistent backing_dev:/dev/ambar/p4 quorum:yes
      open:no blocked:no
  lima-dv-2 node-id:1 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:Inconsistent resync-suspended:no
  lima-dv-3 node-id:2 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:Diskless peer-client:yes
        resync-suspended:no

rc=0
$ [dv-3] drbdsetup status p4 --verbose
p4 node-id:2 role:Secondary suspended:no force-io-failures:no
  volume:0 minor:1004 disk:Diskless client:yes backing_dev:none quorum:yes open:no
      blocked:no
  lima-dv-1 node-id:0 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:Inconsistent resync-suspended:no
  lima-dv-2 node-id:1 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:Inconsistent resync-suspended:no

rc=0
```

Итог: на трёх узлах, где ресурса в ядре не было, `drbdadm adjust p4` вернул `rc=0` и без вывода; после него ресурс настроен: на `dv-1` сразу `disk:Inconsistent quorum:no`, обе связи `Connecting`; после `adjust` на `dv-2` и `dv-3` (через `sleep 5`) все связи `Connected`, у `dv-3` `disk:Diskless client:yes`, на сторонах с диском `Inconsistent` (начальная синхронизация не пропускалась). Отдельный `up` не потребовался. Один прогон.

### P5. Сторона без диска добавляется к работающему ресурсу с писателем

Вопрос: что нужно сделать на узлах, чтобы к поднятому ресурсу из двух копий, на котором идёт запись, добавилась третья сторона без диска; прерывается ли запись; что делает кворум. Ресурс `p5`: `dv-1` (`node-id` 0) и `dv-2` (`node-id` 1), `create-md --max-peers=7`, `up`, `new-current-uuid --clear-bitmap`; на `dv-1` ext4, `probe.sh` пишет и вызывает `sync` на каждую запись, `watch.sh` печатает номер каждые ~0.5 с. Затем файл ресурса переписан с тремя сторонами (`dv-3`, `node-id` 2, `disk none`), `adjust` на `dv-1` и `dv-2`, `up` на `dv-3`. Узлы работают в одной временной зоне, метки времени в выводе — по часам узлов.

```bash
stand/spike/pre-agent.sh p5
```

```text
### P5: two-node resource with a writer, then a node without a disk is added
$ [dv-1] drbdadm create-md --force --max-peers=7 p5
initializing bitmap (224 KB) to all zero
initializing activity log
Writing meta data...
New drbd meta data block successfully created.
rc=0
$ [dv-1] drbdadm up p5
rc=0
$ [dv-2] drbdadm create-md --force --max-peers=7 p5
initializing bitmap (224 KB) to all zero
initializing activity log
Writing meta data...
New drbd meta data block successfully created.
rc=0
$ [dv-2] drbdadm up p5
rc=0
$ [dv-1] drbdadm new-current-uuid --clear-bitmap p5/0
rc=0
$ [dv-1] drbdsetup status p5 --verbose
p5 node-id:0 role:Secondary suspended:no force-io-failures:no
  volume:0 minor:1005 disk:UpToDate backing_dev:/dev/ambar/p5 quorum:yes open:no
      blocked:no
  lima-dv-2 node-id:1 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no

rc=0
08:49:38.169
803

### P5: the resource file now lists three nodes; adjust on the two running nodes
08:49:40.653
$ [dv-1] drbdadm adjust p5
rc=0
$ [dv-2] drbdadm adjust p5
rc=0

### P5: the new node brings the resource up without a disk
$ [dv-3] drbdadm up p5
rc=0
$ [dv-1] drbdsetup status p5 --verbose
p5 node-id:0 role:Primary suspended:no force-io-failures:no
  volume:0 minor:1005 disk:UpToDate backing_dev:/dev/ambar/p5 quorum:yes open:yes
      blocked:no
  lima-dv-2 node-id:1 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:24 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
  lima-dv-3 node-id:2 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:Diskless peer-client:yes
        resync-suspended:no

rc=0
$ [dv-2] drbdsetup status p5 --verbose
p5 node-id:1 role:Secondary suspended:no force-io-failures:no
  volume:0 minor:1005 disk:UpToDate backing_dev:/dev/ambar/p5 quorum:yes open:no
      blocked:no
  lima-dv-1 node-id:0 connection:Connected role:Primary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
  lima-dv-3 node-id:2 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:Diskless peer-client:yes
        resync-suspended:no

rc=0
$ [dv-3] drbdsetup status p5 --verbose
p5 node-id:2 role:Secondary suspended:no force-io-failures:no
  volume:0 minor:1005 disk:Diskless client:yes backing_dev:none quorum:yes open:no
      blocked:no
  lima-dv-1 node-id:0 connection:Connected role:Primary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
  lima-dv-2 node-id:1 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no

rc=0

### P5: did the writer stall or fail while the node was added?
08:49:38.827 acked=855 probe=alive:S
08:49:39.338 acked=904 probe=alive:R
08:49:39.850 acked=971 probe=alive:S
08:49:40.363 acked=1042 probe=alive:S
08:49:40.874 acked=1114 probe=alive:S
08:49:41.385 acked=1201 probe=alive:S
08:49:41.911 acked=1272 probe=alive:S
08:49:42.433 acked=1347 probe=alive:S
08:49:42.969 acked=1470 probe=alive:S
08:49:43.484 acked=1546 probe=alive:S
08:49:44.001 acked=1596 probe=alive:S
08:49:44.519 acked=1658 probe=alive:S
08:49:45.031 acked=1780 probe=alive:S
08:49:45.543 acked=1950 probe=alive:S
08:49:46.053 acked=2080 probe=alive:S
08:49:46.561 acked=2258 probe=alive:S
08:49:47.070 acked=2348 probe=alive:S
08:49:47.580 acked=2420 probe=alive:S
08:49:48.107 acked=2492 probe=alive:S
08:49:48.622 acked=2580 probe=alive:S
08:49:49.131 acked=2775 probe=alive:S
08:49:49.640 acked=2862 probe=alive:S
08:49:50.149 acked=3035 probe=alive:S
08:49:50.659 acked=3226 probe=alive:S
08:49:51.166 acked=3414 probe=alive:S
08:49:51.676 acked=3542 probe=alive:S
08:49:52.190 acked=3604 probe=alive:S
08:49:52.705 acked=3658 probe=alive:R
08:49:53.224 acked=3733 probe=alive:S
08:49:53.745 acked=3783 probe=alive:R
08:49:54.268 acked=3844 probe=alive:S
08:49:54.803 acked=3867 probe=alive:S
08:49:55.332 acked=3916 probe=alive:S
08:49:55.847 acked=3966 probe=alive:S
08:49:56.366 acked=4038 probe=alive:S
08:49:56.883 acked=4079 probe=alive:S
08:49:57.405 acked=4103 probe=alive:S
08:49:57.922 acked=4158 probe=alive:S
08:49:58.452 acked=4186 probe=alive:S
08:49:58.964 acked=4195 probe=alive:S
[Fri Nov 13 08:49:32 2026] drbd p5/0 drbd1005: gen-obligation: MINTING(promoted) -> DISCHARGED(promoted)
[Fri Nov 13 08:49:32 2026] drbd p5: Preparing cluster-wide state change 2303985897: 0->all role( Secondary )
[Fri Nov 13 08:49:32 2026] drbd p5: State change 2303985897: primary_nodes=0, weak_nodes=0
[Fri Nov 13 08:49:32 2026] drbd p5: Committing cluster-wide state change 2303985897 (0ms)
[Fri Nov 13 08:49:32 2026] drbd p5: role( Primary -> Secondary ) [mkfs.ext4:82655 auto-demote]
[Fri Nov 13 08:49:32 2026] drbd p5: Preparing cluster-wide state change 1021233363: 0->all role( Primary )
[Fri Nov 13 08:49:32 2026] drbd p5: State change 1021233363: primary_nodes=1, weak_nodes=FFFFFFFFFFFFFFFC
[Fri Nov 13 08:49:32 2026] drbd p5: Committing cluster-wide state change 1021233363 (4ms)
[Fri Nov 13 08:49:32 2026] drbd p5: role( Secondary -> Primary ) [mount:82684 auto-promote]
[Fri Nov 13 08:49:32 2026] drbd p5/0 drbd1005: gen-obligation: DISCHARGED(promoted) -> ARMED(promoted)
[Fri Nov 13 08:49:32 2026] drbd p5/0 drbd1005: gen-obligation: ARMED(promoted) -> MINTING(promoted)
[Fri Nov 13 08:49:32 2026] drbd p5/0 drbd1005: gen-obligation: MINTING(promoted) -> DISCHARGED(promoted)
[Fri Nov 13 08:49:41 2026] drbd p5 lima-dv-3: Starting sender thread (peer-node-id 2)
[Fri Nov 13 08:49:41 2026] drbd p5 lima-dv-3: conn( StandAlone -> Unconnected ) [connect]
[Fri Nov 13 08:49:41 2026] drbd p5 lima-dv-3: Starting receiver thread (peer-node-id 2)
[Fri Nov 13 08:49:41 2026] drbd p5 lima-dv-3: conn( Unconnected -> Connecting ) [connecting]
[Fri Nov 13 08:49:44 2026] drbd p5 lima-dv-3: Handshake to peer 2 successful: Agreed network protocol version 125
[Fri Nov 13 08:49:44 2026] drbd p5 lima-dv-3: Feature flags enabled on protocol level: 0x7ff TRIM THIN_RESYNC WRITE_SAME WRITE_ZEROES RESYNC_DAGTAG WRITE_POSTPONE
[Fri Nov 13 08:49:44 2026] drbd p5: Preparing cluster-wide state change 857000934: 0->2 role( Primary ) conn( Connected )
[Fri Nov 13 08:49:45 2026] drbd p5/0 drbd1005 lima-dv-3: self AA3F417BDC3399FA:0000000000000000:0000000000000000:0000000000000000 bits:0 flags:0
[Fri Nov 13 08:49:45 2026] drbd p5/0 drbd1005 lima-dv-3: peer's exposed UUID: 0000000000000000
[Fri Nov 13 08:49:45 2026] drbd p5: State change 857000934: primary_nodes=1, weak_nodes=FFFFFFFFFFFFFFF8
[Fri Nov 13 08:49:45 2026] drbd p5: Committing cluster-wide state change 857000934 (16ms)
[Fri Nov 13 08:49:45 2026] drbd p5 lima-dv-3: conn( Connecting -> Connected ) peer( Unknown -> Secondary ) [connected]
[Fri Nov 13 08:49:45 2026] drbd p5/0 drbd1005 lima-dv-3: pdsk( DUnknown -> Diskless ) repl( Off -> Established ) [connected]
[Fri Nov 13 08:49:45 2026] drbd p5/0 drbd1005: gen-obligation: DISCHARGED(promoted) -> ARMED(peer-returned-diskless)
[Fri Nov 13 08:49:45 2026] drbd p5: Preparing remote state change 2427081311: 1->2 role( Secondary ) conn( Connected )
[Fri Nov 13 08:49:45 2026] drbd p5/0 drbd1005: gen-obligation: ARMED(peer-returned-diskless) -> MINTING(peer-returned-diskless)
[Fri Nov 13 08:49:45 2026] drbd p5/0 drbd1005: gen-obligation: MINTING(peer-returned-diskless) -> DISCHARGED(peer-returned-diskless)
[Fri Nov 13 08:49:45 2026] drbd p5 lima-dv-2: Committing remote state change 2427081311 (primary_nodes=1)

### P5: stop the writer, compare acknowledged and on-volume numbers, read through the new node
acked=4206 on-volume=4207
on-volume-via-dv-3=4207
```

`dmesg -T` с трёх узлов по ресурсу `p5`. Команда оставляет строки ресурса `p5`, в которых есть `quorum` (в любое время) или метка времени 08:49:33–08:49:59, и убирает строки с `gen-obligation`; снято после опыта и очистки: `dmesg -T | grep -E "drbd p5" | grep -E "quorum|08:49:(3[3-9]|[45])" | grep -vE "gen-obligation"`. Строки с меткой 08:50 попали в листинг только как строки с `quorum`.

```text
== dv-1
[Fri Nov 13 08:49:21 2026] drbd p5/0 drbd1005: quorum( no -> yes ) [connected]
[Fri Nov 13 08:49:41 2026] drbd p5 lima-dv-3: Starting sender thread (peer-node-id 2)
[Fri Nov 13 08:49:41 2026] drbd p5 lima-dv-3: conn( StandAlone -> Unconnected ) [connect]
[Fri Nov 13 08:49:41 2026] drbd p5 lima-dv-3: Starting receiver thread (peer-node-id 2)
[Fri Nov 13 08:49:41 2026] drbd p5 lima-dv-3: conn( Unconnected -> Connecting ) [connecting]
[Fri Nov 13 08:49:44 2026] drbd p5 lima-dv-3: Handshake to peer 2 successful: Agreed network protocol version 125
[Fri Nov 13 08:49:44 2026] drbd p5: Preparing cluster-wide state change 857000934: 0->2 role( Primary ) conn( Connected )
[Fri Nov 13 08:49:45 2026] drbd p5: State change 857000934: primary_nodes=1, weak_nodes=FFFFFFFFFFFFFFF8
[Fri Nov 13 08:49:45 2026] drbd p5 lima-dv-3: conn( Connecting -> Connected ) peer( Unknown -> Secondary ) [connected]
[Fri Nov 13 08:49:45 2026] drbd p5/0 drbd1005 lima-dv-3: pdsk( DUnknown -> Diskless ) repl( Off -> Established ) [connected]
[Fri Nov 13 08:50:11 2026] drbd p5/0 drbd1005: Would lose quorum, but using tiebreaker logic to keep
[Fri Nov 13 08:50:11 2026] drbd p5/0 drbd1005: quorum( yes -> no ) [down]
== dv-2
[Fri Nov 13 08:49:21 2026] drbd p5/0 drbd1005: quorum( no -> yes ) [remote]
[Fri Nov 13 08:49:42 2026] drbd p5 lima-dv-3: Starting sender thread (peer-node-id 2)
[Fri Nov 13 08:49:42 2026] drbd p5 lima-dv-3: conn( StandAlone -> Unconnected ) [connect]
[Fri Nov 13 08:49:42 2026] drbd p5 lima-dv-3: conn( Unconnected -> Connecting ) [connecting]
[Fri Nov 13 08:49:45 2026] drbd p5 lima-dv-3: conn( Connecting -> Connected ) peer( Unknown -> Secondary ) [connected]
[Fri Nov 13 08:49:45 2026] drbd p5/0 drbd1005 lima-dv-3: pdsk( DUnknown -> Diskless ) repl( Off -> Established ) [connected]
[Fri Nov 13 08:50:11 2026] drbd p5/0 drbd1005: Would lose quorum, but using tiebreaker logic to keep
[Fri Nov 13 08:50:20 2026] drbd p5/0 drbd1005: disk( Detaching -> Diskless ) quorum( yes -> no ) [go-diskless]
== dv-3
[Fri Nov 13 08:49:44 2026] drbd p5: Starting worker thread (node-id 2)
[Fri Nov 13 08:49:44 2026] drbd p5 lima-dv-1: conn( StandAlone -> Unconnected ) [connect]
[Fri Nov 13 08:49:44 2026] drbd p5 lima-dv-2: conn( StandAlone -> Unconnected ) [connect]
[Fri Nov 13 08:49:45 2026] drbd p5/0 drbd1005: size = 1024 MB (1048316 KB)
[Fri Nov 13 08:49:45 2026] drbd p5/0 drbd1005: quorum( no -> yes ) [remote]
[Fri Nov 13 08:50:20 2026] drbd p5/0 drbd1005: quorum( yes -> no ) [remote]
```

Листинг сокращён относительно того, что выдала бы команда: в нём для `dv-1` нет строк `Feature flags enabled…`, `Committing cluster-wide state change 857000934` и `self`/`peer's exposed UUID`, которые есть в первом листинге `dmesg` `dv-1` выше (метки 08:49:44–08:49:45). Полный вывод в протокол не включён.

Наблюдаемые факты:

| Факт | Значение | Источник |
|---|---|---|
| `adjust` на `dv-1` | `rc=0`; в `dmesg` `dv-1` соединение с `dv-3` появляется с меткой `[connect]` в 08:49:41 | `date` до `adjust`: 08:49:40.653; `dmesg` `dv-1` |
| `adjust` на `dv-2` | `rc=0`; соединение с `dv-3` с меткой `[connect]` в 08:49:42 | `dmesg` `dv-2` |
| `up` на `dv-3` | `rc=0`; на `dv-3` соединения с `dv-1` и `dv-2` `StandAlone -> Unconnected` в 08:49:44; `Connecting -> Connected` с `dv-3` записано в 08:49:45 у `dv-1` и у `dv-2` | `dmesg` `dv-3` (первое), `dmesg` `dv-1` и `dv-2` (второе) |
| Достаточность | да: после трёх команд на всех узлах три связи `Connected`, `dv-3` `disk:Diskless client:yes quorum:yes`, роль `dv-1` `Primary` | `drbdsetup status p5 --verbose` |
| Необходимость `adjust` на запущенных узлах | без `adjust` не запускалось; в этом прогоне соединение с `dv-3` на `dv-1` и `dv-2` возникло только с командой `adjust` каждого из них `[проверить: нужен ли adjust на каждом из двух узлов по отдельности]` | `dmesg` |
| Остановка писателя | в выборках не видна: в `watch.log` из 40 выборок (39 интервалов по 0.507–0.536 с) номер вырос в каждом интервале, минимальный прирост 9 (4186 → 4195); состояние писателя `S` или `R`, не `D`; `probe.log` пуст. Остановка короче интервала между выборками не была бы видна | `watch.log`, `probe.log` |
| Скорость записи | До `adjust` (08:49:38.827 → 08:49:40.363): 855 → 1042, 187 за 1.536 с, ~122 номера/с. После `Connected` с `dv-3` (метка `dmesg` 08:49:45 с точностью до секунды; первая выборка с такой меткой 08:49:45.031, номер 1780) до 08:49:51.676 (3542): 1762 за 6.645 с, ~265 номеров/с, по интервалам от ~137 до ~383. Спад начался с интервала, закончившегося в 08:49:52.190 (3604, ~121/с): 08:49:51.676 → 08:49:58.964 (4195) прирост 653 за 7.288 с, ~90/с; последние интервалы 08:49:56.366 → 08:49:58.964: ~79, ~46, ~106, ~53, ~18/с, в последних пяти интервалах скорость ниже 110/с и колеблется (~18–106/с). Один прогон, одна выборка за ~0.5 с; общего «до/после» нет `[проверить: причина падения скорости — связь с dv-3, нагрузка ВМ или ext4]` | `watch.log` |
| Кворум | у `dv-1` и `dv-2` нет строк `quorum(` между 08:49:21 и 08:50:11, `quorum:yes` в `drbdsetup status`; `quorum( no -> yes )` у `dv-3` при подключении к двум сторонам с диском; строки 08:50:11–08:50:20 (`Would lose quorum, but using tiebreaker logic to keep`, `quorum( yes -> no )`) совпадают по времени с очисткой после опыта `[проверить: причина — остановка ресурса очисткой]` | `dmesg` |
| Подтверждено / на томе | последний подтверждённый номер 4206, последний номер на томе 4207 (`dv-1`); на `dv-3` после размонтирования на `dv-1` тоже 4207. Сплошность журнала (нет ли пропущенных номеров) не сверялась | вывод опыта |

Итог: `drbdadm adjust` на двух работающих узлах после обновления файла ресурса и `drbdadm up` на новом узле были достаточны для подключения стороны без диска; необходимость каждого шага не проверялась. В выборках через ~0.5 с номер писателя рос каждый раз, ошибок в `probe.log` нет; остановка короче интервала не видна. Строк `quorum(` на узлах с копией при добавлении стороны (08:49:21–08:50:11) нет, `quorum:yes`; как добавление стороны меняет расчёт кворума, не изучалось `[проверить: расчёт кворума при добавлении стороны]`. Последний подтверждённый номер (4206) не больше последнего номера на томе (4207), читалось на `dv-1` и через `dv-3`; сплошность журнала не сверялась, причина расхождения на единицу в записи не зафиксирована `[проверить: причина расхождения 4206/4207]`. Один прогон, одна пара `adjust`; порядок «`up` на новом узле после `adjust` на старых» не менялся.

### P6. `drbdadm up` для файла без метаданных

Вопрос: отказывает ли `up`, когда в файле нет метаданных DRBD. Ресурс `p6`: файл 1 ГиБ без `create-md`, loop-устройство, `up` на `dv-1`.

```bash
stand/spike/pre-agent.sh p6
```

```text
### P6: drbdadm up on a backing file without metadata
$ [dv-1] drbdadm up p6
rc=1
No valid meta data found
Command 'drbdmeta 1006 v09 /dev/ambar/p6 internal repair-md' terminated with exit code 255
$ [dv-1] drbdsetup status p6
p6 role:Secondary
  disk:Diskless client:no quorum:no open:no
  lima-dv-2 connection:StandAlone
  lima-dv-3 connection:StandAlone

rc=0
$ [dv-1] drbdadm adjust p6
No valid meta data found
rc=1
Command 'drbdmeta 1006 v09 /dev/ambar/p6 internal repair-md' terminated with exit code 255
```

`dmesg -T` на `dv-1` по ресурсу `p6` (`dmesg -T | grep -E "drbd p6|drbd1006" | tail -n 25`):

```text
[Fri Nov 13 08:50:56 2026] drbd p6: Starting worker thread (node-id 0)
[Fri Nov 13 08:50:56 2026] drbd p6 lima-dv-2: Starting sender thread (peer-node-id 1)
[Fri Nov 13 08:50:56 2026] drbd p6 lima-dv-3: Starting sender thread (peer-node-id 2)
[Fri Nov 13 08:51:05 2026] drbd p6 lima-dv-2: Terminating sender thread
[Fri Nov 13 08:51:05 2026] drbd p6 lima-dv-3: Terminating sender thread
```

Итог: `drbdadm up p6` вернул `rc=1` с текстом `No valid meta data found` и `Command 'drbdmeta 1006 v09 /dev/ambar/p6 internal repair-md' terminated with exit code 255`. Ресурс при этом остался в ядре: `drbdsetup status p6` печатает `disk:Diskless client:no quorum:no`, обе связи `StandAlone`, `rc=0`; `drbdadm adjust p6` повторил ту же ошибку (`rc=1`). Для возврата в исходное состояние скрипт выполнил `drbdsetup down p6` (очистка после опыта). Состояние ресурса после неудачного `up` — наполовину настроенное, не отсутствующее. Один прогон, один узел.

### Очистка стенда после опытов P1–P6

```bash
stand/spike/pre-agent.sh clean && stand/spike/clean.sh && stand/check.sh
```

```text
OK: stand is up
```

На трёх узлах `drbdsetup status` — `# No currently configured DRBD found.`, `losetup -l -n | wc -l` = 0, `/dev/ambar` и `/mnt/pa` отсутствуют, в `/var/tmp` только `cloud-init` и каталог `systemd-private-…`, `/sys/module/drbd/version` = `9.3.4`.
