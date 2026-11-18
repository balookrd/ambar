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

## Опыты перед CSI (P7–P10)

Дата: 2026-11-13. Стенд тот же: три ВМ Lima, ядро `6.12.116+deb13-arm64`, DRBD 9.3.4, drbd-utils 9.35.0 (версии заново не снимались). Опыты выполняет `stand/spike/pre-csi.sh <p7|p8|p9|p10>`: перед опытом и после него скрипт останавливает ресурсы `p7`–`p10`, отсоединяет loop-устройства и удаляет файлы. Файлы ресурсов содержат `options { quorum majority; on-no-quorum io-error; auto-promote yes; }`. P7 и P8 запущены по одному разу, P9 — дважды (см. «Отклонения и правки скрипта»), P10 — три раза (см. P10 ниже). Метки времени в выводе P8 — по часам узлов. После записанных прогонов скрипт доработан (очистка при сбое через `trap … EXIT`, ожидание писателей перед `umount`, проверка кодов возврата подготовительных шагов и наличия строки `rc=`, `mktemp` вместо фиксированных путей); доработанная версия не запускалась `[проверить: прогон `pre-csi.sh p7`, `p8`, `p9` после правки]`.

| Опыт | Условия | Команда | Код возврата | Вывод | Прогонов |
|---|---|---|---|---|---|
| P7 (1) | `dv-1`, файл 1 ГиБ без метаданных, ресурс не поднят | `drbdadm dump-md p7` | 255 | stdout пуст; stderr: `No valid meta data found`, `Command 'drbdmeta 1007 v09 /dev/ambar/p7 internal dump-md' terminated with exit code 255` | 1 |
| P7 (2) | после `drbdadm create-md --force --max-peers=7 p7` | `drbdadm dump-md p7` | 0 | дамп метаданных (`version "v09"; max-peers 7;`, `md_offset 1073737728`), stderr пуст | 1 |
| P7 (3) | после `drbdadm up p7` | `drbdadm dump-md p7` | 20 | stdout: `Operation canceled.`; stderr: `open(/dev/ambar/p7) failed: Device or resource busy`, `Exclusive open failed. Do it anyways?`, `stdin not a TTY, not waiting for confirmation`, `Command 'drbdmeta 1007 v09 /dev/ambar/p7 internal dump-md' terminated with exit code 20` | 1 |
| P8 (1) | `down p8` на `dv-3`, `p8.res` на `dv-3` удалён, на `dv-1` идёт запись | `drbdsetup status p8 --verbose` на `dv-1`, `dv-2` | 0 | связь с `lima-dv-3`: `connection:Connecting role:Unknown`, `peer-disk:DUnknown` на обоих; `quorum:yes` | 1 |
| P8 (2) | `p8.res` без `dv-3` на `dv-1` и `dv-2` | `drbdadm adjust p8` на `dv-1`, затем на `dv-2` | 0, 0 | вывода нет; в статусе обоих узлов остался только пир `Connected`, секции `lima-dv-3` нет; в `dmesg` `lima-dv-3: conn( Connecting -> Disconnecting ) [del-peer]`, затем `Disconnecting -> StandAlone` | 1 |
| P8 (3) | то же | журнал `watch.sh`, `dmesg`, `quorum` | — | в 40 последних выборках `watch.sh` (19:47:03–19:47:22), включая момент `adjust` (19:47:13.235), `probe=alive:S`, `acked` растёт от выборки к выборке; лог `probe.sh` (stdout и stderr) пуст; в окне `dmesg \| tail -n 60` с 19:46:45 строк `quorum( yes -> no )` нет; `quorum:yes` на обоих узлах | 1 |
| P8 (4) | файл с тремя сторонами на всех, `adjust` на `dv-1`, `dv-2`, `up` на `dv-3` | `drbdsetup status p8 --verbose --statistics` | 0 на трёх узлах | три связи `Connected`, `dv-3`: `node-id:2`, `Diskless client:yes`; `out-of-sync:0` на всех связях; `bm-writes:112` на `dv-1` и `dv-2` | 1 |
| P8 (5) | писатель остановлен | подтверждённый номер и последний номер на томе | — | `acked=10875 on-volume=10876` | 1 |
| P9 (1) | `p9`, три копии, синхронизация пропущена, ФС нет, чтение на `dv-2` | `blkid -p -o value -s TYPE /dev/drbd1009` | 2 | stdout и stderr пусты | код возврата — 2, stdout и stderr — 1 |
| P9 (1) | то же | `dd if=/dev/drbd1009 bs=1 skip=1080 count=2 status=none \| od -An -tx1` | 0 | ` 00 00` | 1 |
| P9 (2) | `mkfs.ext4 -q` на `dv-1`, не смонтирован | `blkid -p -o value -s TYPE /dev/drbd1009` на `dv-2` | 0 | stdout `ext4`, stderr пуст | код возврата — 2, stdout и stderr — 1 |
| P9 (2) | то же | `dd … \| od -An -tx1` на `dv-2` | 0 | ` 53 ef` | 1 |
| P9 (3) | том смонтирован на `dv-1` (Primary) | `blkid -p -o value -s TYPE /dev/drbd1009` на `dv-2` | 2 | stdout пуст; stderr `blkid: error: /dev/drbd1009: Wrong medium type` | код возврата — 2, stdout и stderr — 1 |
| P9 (3) | то же | `dd … \| od -An -tx1` на `dv-2` (с `set -o pipefail`) | 1 | stderr `dd: failed to open '/dev/drbd1009': Wrong medium type`; stdout `od` пуст | 1 |
| P9 (4) | `/dev/drbd1999` не существует (`ls -l`: код 2, `No such file or directory`) | `blkid -p -o value -s TYPE /dev/drbd1999` | 2 | stdout и stderr пусты | код возврата — 2, stdout и stderr — 1 |
| P9 (4) | то же | `dd … \| od -An -tx1` на `dv-2` (с `set -o pipefail`) | 1 | stderr `dd: failed to open '/dev/drbd1999': No such file or directory` | 1 |
| P10 (1) | `p10` (minor 1010, порт 7010, 1 ГиБ, `--max-peers=7`, три узла с копией), сразу после `create-md --force --max-peers=7`, ресурс не поднимался | `drbdadm dump-md p10` на `dv-1` | 0 | `node-id -1`, `current-uuid 0x0000000000000004`, `flags 0x00000080`, `members 0x0000000000000000`; `peer[0]`–`peer[31]`: `bitmap-index -1`, `bitmap-uuid 0x0000000000000000`, `flags 0x00000000`; `history-uuids` — 32 значения `0x0000000000000000`; `la-size-sect 0`; все `# bits-set[0..6]` равны 0 | 2 |
| P10 (2) | после `up` и `down` на `dv-1`; `dv-2` и `dv-3` не настраивались | `drbdadm dump-md p10`, затем `apply-al`, затем снова `dump-md` | 255, 255 (через 5 с), 0 (`apply-al`), 0 | до `apply-al` stdout пуст, stderr `Found meta data is "unclean", please apply-al first`; после: `node-id 0`, `current-uuid 0x0000000000000004`, `flags 0x00000480`, `members 0x0000000000000001`; `peer[1]` `bitmap-index 1`, `peer[2]` `bitmap-index 0`, у всех слотов `bitmap-uuid` и `flags` равны 0; `history-uuids` нули, `la-size-sect 0` | 2 |
| P10 (3) | три узла подняты, связи `Connected`, диски `Inconsistent`, пропуска синхронизации нет, затем `down` на `dv-1` | то же | 255, 255, 0, 0 | после `apply-al`: `current-uuid 0x0000000000000004`, `flags 0x00000480`, `bitmap-uuid` всех слотов 0, `history-uuids` нули, `bits-set` нули, `la-size-sect 0`; `peer[0]` `flags 0x00010000 # exists`, `peer[1]` и `peer[2]` `flags 0x00000052 # <=outdated \| seen \| 0x40=?` | 2 |
| P10 (4) | `dv-1` поднят снова, `new-current-uuid --clear-bitmap p10/0` на `dv-1`, все три `UpToDate`, затем `down` на `dv-1` | то же | 255, 255, 0, 0 | после `apply-al`: `flags 0x00000091`; `current-uuid` прогон 1 `0x0240DDD21A4B2CE8`, прогон 2 `0x43522DBE2645857A`; `peer[3]`–`peer[31]`: `bitmap-uuid` прогон 1 `0x5400D259A0DF9D13`, прогон 2 `0x7C55463636456D08`, `flags 0x00000020 # divergence`; `peer[1]`, `peer[2]` `flags 0x00000010 # seen`; `history-uuids` нули; `la-size-sect 2096632` | 2 |
| P10 (5) | `dv-1` поднят снова, `mkfs.ext4 -q` и файл на `dv-2`, все `UpToDate`, затем `down` на `dv-1` | то же | 255, 255, 0, 0 | `flags 0x00000091`; `la-size-sect 2096632`; `current-uuid` прогон 1 `0x5E59F70730D0DDAC`, прогон 2 `0xA6C47762AA5BC552` (только в таблице); `history-uuids[0]` равен `current-uuid` состояния (4) (прогон 1 `0x0240DDD21A4B2CE8`), остальные нули; `# bits-set[2..6] 262079`; `peer[3]`–`peer[31]` `bitmap-uuid` ненулевой (прогон 1 `0x5400D259A0DF9D13`, прогон 2 `0x7C55463636456D08`, только в таблице) | 2 |
| P10 (6) | `create-md --force --max-peers=7` на `dv-1` при `UpToDate` у `dv-2` и `dv-3`, `up`, полная синхронизация, `down` | то же | 255, 255, 0, 0 | `flags 0x00000091`; `la-size-sect 2096632`; `current-uuid` равен `current-uuid` состояния (5) у обоих прогонов; `device-uuid` другой, чем в (1)–(5); `# bits-set[0] 1024`, `# bits-set[2..6] 262079`; `peer[3]`–`peer[31]` `bitmap-uuid` прогон 1 `0xDFB51D0831B0A7DA`, прогон 2 `0x189AE62E9C528BD0` (только в таблице) | 2 |

Наблюдения в пределах записанного:

- P7: код возврата 255 без метаданных, 0 при метаданных на неподнятом ресурсе, 20 после `up`; команда запускалась без TTY и с закрытым stdin (`</dev/null`); код 20 и сообщение `stdin not a TTY, not waiting for confirmation` в состоянии (3) получены при этих условиях, поведение с TTY не запускалось `[проверить: `drbdadm dump-md` на поднятом ресурсе с TTY]`.
- P8: после `adjust` на `dv-1` и `dv-2` соединение с `dv-3` исчезло из статуса обоих узлов. В 40 последних выборках `watch.sh` (19:47:03–19:47:22, включая `adjust` в 19:47:13) `acked` рос от выборки к выборке, `quorum` оставался `yes`. Токен `S` в `probe=alive:S` — поле `State:` из `/proc/<pid>/status` процесса `probe.sh` (`S` — спит в ожидании; `D` — заблокирован в ядре, см. комментарий в `stand/spike/watch.sh`). После возврата `dv-3` с тем же `node-id 2` соединения стали `Connected`. Остановка короче интервала выборки `watch.sh` (~0.5 с) не видна.
- P8, размер битовой карты (шаг 4 задания): единственное записанное значение — `initializing bitmap (224 KB) to all zero` в выводе `create-md` на `dv-1` и `dv-2`. `drbdsetup status p8 --verbose --statistics` поля с размером битовой карты не печатает; связанные с битовой картой поля в выводе — `bm-writes:112` (на `dv-1` и `dv-2`, 0 на `dv-3`) и `out-of-sync:0` на всех связях; `size:1048316` — размер устройства в КБ. Размера битовой карты после повторного подключения `dv-3` в записанном выводе нет `[проверить: размер битовой карты после повторного подключения стороны]`.
- P8, сравнение номеров (шаг 5 задания): `acked=10875 on-volume=10876`, номер на томе на единицу больше подтверждённого. `on-volume` прочитан командой `tail -n 1 /mnt/pa/log` на `dv-1` с файловой системы, смонтированной на `/dev/drbd1008`; копия на `dv-2` не читалась.
- P8, запись во время повторного подключения: записанный хвост `watch.sh` после возврата `dv-3` начинается в 19:47:33.237 и не покрывает 19:47:27–31 (`adjust` в 19:47:27.567, handshake с `dv-3` в 19:47:31 по `dmesg`), поэтому продолжалась ли запись в этот интервал, вывод не показывает `[проверить: запись во время повторного подключения стороны]`.
- P9: в состоянии (3) на `dv-2` и `blkid`, и `dd` получили `Wrong medium type` (на `dv-2` роль `Secondary`, `open:no`). Код возврата `blkid` равен 2 в состояниях (1), (3) и (4). Состояние (3) отличается от (1) и (4) по stderr: `Wrong medium type` в (3), пусто в (1) и (4). Состояния (1) и (4) отличаются только вторым признаком, чтением `dd`: в (1) `dd` открывает устройство и читает `00 00`, в (4) не может открыть его (`No such file or directory`, код 1). В (4) `blkid` ничего не печатает. Отсутствие устройства проверено для одного имени, `/dev/drbd1999`.

- P10, повторы: скрипт запущен трижды. Первый прогон (скрипт без `apply-al`) дал `dump-md` с кодом 255 и сообщением `Found meta data is "unclean", please apply-al first` в состояниях (2)–(6), сам вывод в протокол не включён. После добавления `apply-al` в скрипт выполнены два прогона, оба полностью приведены в числах таблицы; сырой вывод — прогон 1 целиком, прогон 2 только для состояний (1) и (4). Состояния (1)–(6) в обоих прогонах дали одинаковые коды и одинаковые значения полей, кроме значений, которые DRBD генерирует случайно (`device-uuid`, `current-uuid` после (3), `bitmap-uuid` слотов 3–31, метка времени в заголовке).
- P10, `dump-md` после `up`: всего 15 случаев (состояния (2)–(6), три прогона скрипта). Сырой вывод первого `dump-md` после `drbdadm down` приведён для 6 случаев: прогон 1 (состояния (2)–(6)) и прогон 2 (состояние (4)); в каждом из них код 255, stderr `Found meta data is "unclean", please apply-al first`, stdout пуст; повторный через 5 с — тот же результат; после `drbdadm apply-al p10` — код 0 с полным дампом. Остальные 9 случаев (скрипт без `apply-al`: состояния (2)–(6); прогон 2: состояния (2), (3), (5), (6)) описаны по журналам прогонов, которых в протокол нет: для первых пяти известны только код 255 и сообщение, для четырёх остальных — коды из столбца таблицы. Причина флага AL `unclean` после штатного `down` не выяснялась `[проверить: почему метаданные после `drbdadm down` считаются unclean]`. Состояние (1) (без `up`) дало код 0 без `apply-al`.
- P10, поля, одинаковые в состояниях (1)–(3) и отличающиеся в (4)–(6): `current-uuid` — `0x0000000000000004` в (1)–(3) у обоих прогонов, другое значение в (4)–(6); верхние `flags` — `0x00000080` в (1), `0x00000480` в (2) и (3), `0x00000091` в (4)–(6); `bitmap-uuid` слотов 3–31 — `0x0000000000000000` в (1)–(3), ненулевой в (4)–(6) (значение меняется от прогона к прогону); `flags` слотов 3–31 — `0x00000000` в (1)–(3), `0x00000020 # divergence` в (4)–(6); `la-size-sect` — `0` в (1)–(3), `2096632` в (4)–(6).
- P10, поля, которые состояния (1)–(3) от (4)–(6) не разделяют: `history-uuids` нули не только в (1)–(3), но и в (4); `members` — `0x0000000000000001` в (2)–(6); `features` — `0x0000000000000001` во всех; `bitmap-uuid` слотов 0–2 — нули во всех состояниях; `bitmap-dagtag` — нули во всех; `la-peer-max-bio-size` — `0` в (1), `1048576` в (2)–(6). `node-id`, `bitmap-index` слотов 1 и 2 и `members` в (1) отличаются от (2) и (3): они меняются после первого `up` и идентификатором данных не являются (вывод только о том, что значения различаются).
- P10, состояние (3) отличается от (2) только флагами слотов: `peer[0]` `0x00010000 # exists`, `peer[1]` и `peer[2]` `0x00000052`; `current-uuid`, все `bitmap-uuid`, `history-uuids` и `bits-set` те же, что в (1) и (2). Значение бита `0x40` в выводе помечено `0x40=?`, смысл битов верхних `flags` в выводе не расшифрован `[проверить: смысл битов 0x400 и 0x40]`.
- P10, границы вывода: в состояниях (1)–(3) на устройство DRBD не писали и `primary --force` не вызывали. Данные, записанные в файл напрямую в обход DRBD, `dump-md` не показывает: он читает только область метаданных `[проверить: дамп для файла, в который писали напрямую при нетронутых метаданных]`. Размер дампа: 249–269 строк; слотов `peer[n]` 32 при `max-peers 7`, `history-uuids` — 32 значения, `bitmap[0..6]` — 7 блоков.

### Отклонения и правки скрипта

| Место | Что произошло |
|---|---|
| `run_split`, первый прогон P9 | команда `$* >/tmp/rs.out 2>/tmp/rs.err </dev/null` применяла `</dev/null` к последней команде конвейера: `od` читал пустой stdin, строки `od` в состояниях (1) и (2) были пусты, а ошибка `dd` в (3) и (4) попала в stdout пода вместо файла. Команда обёрнута в `{ set -o pipefail; …; }`, редирект относится к группе. P9 запущен заново целиком; в протоколе вывод второго прогона. Коды `blkid` первого прогона: (1) 2, (2) 0, (3) 2, (4) 2, как во втором. Вывод первого прогона в протокол не включён |
| P7, P8 | выполнены до правки `run_split`. В P7 нет конвейеров, P8 `run_split` не использует; правка на них не влияет |
| P9, код возврата `dd` | в первом прогоне записан код `od` (0), во втором с `pipefail` — код `dd` (1) |
| Диагностика P9 | при разборе пустого вывода `od` ресурс `p9` поднимался отдельным отладочным скриптом (не входит в `pre-csi.sh`); `dd … \| od -An -tx1` на готовой ext4 на `dv-2` дал ` 53 ef` в пяти запусках `[проверить: вывод не сохранён]`. После отладки выполнены `pre-csi.sh clean` и `stand/spike/clean.sh` |

### P7. `drbdadm dump-md`

Ресурс `p7`: `dv-1` (`node-id` 0, `disk /dev/ambar/p7; meta-disk internal`), `dv-2` и `dv-3` только в файле, minor 1007, port 7007.

```bash
stand/spike/pre-csi.sh p7
```

```text

### P7: drbdadm dump-md in three states, resource p7 on dv-1

### P7 (1): empty sparse file, no metadata
$ [dv-1] drbdadm dump-md p7
rc=255
--- stdout (first 20 lines)
--- stderr
No valid meta data found
Command 'drbdmeta 1007 v09 /dev/ambar/p7 internal dump-md' terminated with exit code 255

### P7 (2): after create-md --force --max-peers=7
$ [dv-1] drbdadm create-md --force --max-peers=7 p7
initializing bitmap (224 KB) to all zero
initializing activity log
Writing meta data...
New drbd meta data block successfully created.
rc=0
$ [dv-1] drbdadm dump-md p7
rc=0
--- stdout (first 20 lines)
# DRBD meta data dump
# 2026-11-13 11:46:03 +0000 [1794570363]
# lima-dv-1> drbdmeta 1007 v09 /dev/ambar/p7 internal dump-md
#

version "v09";

max-peers 7;
# md_size_sect 520
# md_offset 1073737728
# al_offset 1073704960
# bm_offset 1073475584

node-id -1;
current-uuid 0x0000000000000004;
flags 0x00000080;
members 0x0000000000000000;
features 0x0000000000000001;
peer[0] {
    bitmap-index -1;
--- stderr

### P7 (3): after drbdadm up (disk attached)
$ [dv-1] drbdadm up p7
rc=0
$ [dv-1] drbdadm dump-md p7
rc=20
--- stdout (first 20 lines)
Operation canceled.
--- stderr
open(/dev/ambar/p7) failed: Device or resource busy

Exclusive open failed. Do it anyways?
stdin not a TTY, not waiting for confirmation
Command 'drbdmeta 1007 v09 /dev/ambar/p7 internal dump-md' terminated with exit code 20
$ [dv-1] drbdsetup status p7 --verbose
p7 node-id:0 role:Secondary suspended:no force-io-failures:no
  volume:0 minor:1007 disk:Inconsistent backing_dev:/dev/ambar/p7 quorum:no open:no
      blocked:no
  lima-dv-2 node-id:1 connection:Connecting role:Unknown tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Off peer-disk:DUnknown resync-suspended:no
  lima-dv-3 node-id:2 connection:Connecting role:Unknown tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Off peer-disk:DUnknown resync-suspended:no

rc=0
```

### P8. Отключение стороны без диска от работающего ресурса

Ресурс `p8`: `dv-1` (`node-id` 0) и `dv-2` (`node-id` 1) с копией, `dv-3` (`node-id` 2) без диска, `create-md --max-peers=7`, `new-current-uuid --clear-bitmap`, ext4 смонтирована на `dv-1`, `probe.sh` и `watch.sh` запущены как в P5.

```bash
stand/spike/pre-csi.sh p8
```

```text

### P8: two copies and a diskless node, writer on dv-1; the diskless node is removed
$ [dv-1] drbdadm create-md --force --max-peers=7 p8
initializing bitmap (224 KB) to all zero
initializing activity log
Writing meta data...
New drbd meta data block successfully created.
rc=0
$ [dv-2] drbdadm create-md --force --max-peers=7 p8
initializing bitmap (224 KB) to all zero
initializing activity log
Writing meta data...
New drbd meta data block successfully created.
rc=0
$ [dv-1] drbdadm up p8
rc=0
$ [dv-2] drbdadm up p8
rc=0
$ [dv-3] drbdadm up p8
rc=0
$ [dv-1] drbdadm new-current-uuid --clear-bitmap p8/0
rc=0
$ [dv-1] drbdsetup status p8 --verbose
p8 node-id:0 role:Secondary suspended:no force-io-failures:no
  volume:0 minor:1008 disk:UpToDate backing_dev:/dev/ambar/p8 quorum:yes open:no
      blocked:no
  lima-dv-2 node-id:1 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
  lima-dv-3 node-id:2 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:Diskless peer-client:yes
        resync-suspended:no

rc=0
19:47:02.869
979

### P8 step 1: down on dv-3, delete its p8.res
19:47:03.023
$ [dv-3] drbdadm down p8
rc=0
ls: cannot access '/var/lib/rancher/ambar/res/p8.res': No such file or directory
$ [dv-1] drbdsetup status p8 --verbose
p8 node-id:0 role:Primary suspended:no force-io-failures:no
  volume:0 minor:1008 disk:UpToDate backing_dev:/dev/ambar/p8 quorum:yes open:yes
      blocked:no
  lima-dv-2 node-id:1 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:24 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
  lima-dv-3 node-id:2 connection:Connecting role:Unknown tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Off peer-disk:DUnknown resync-suspended:no

rc=0
$ [dv-2] drbdsetup status p8 --verbose
p8 node-id:1 role:Secondary suspended:no force-io-failures:no
  volume:0 minor:1008 disk:UpToDate backing_dev:/dev/ambar/p8 quorum:yes open:no
      blocked:no
  lima-dv-1 node-id:0 connection:Connected role:Primary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
  lima-dv-3 node-id:2 connection:Connecting role:Unknown tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Off peer-disk:DUnknown resync-suspended:no

rc=0

### P8 step 2: p8.res without dv-3 on dv-1 and dv-2, adjust
resource p8 {
    options { quorum majority; on-no-quorum io-error; auto-promote yes; }
    net { protocol C; }
    on lima-dv-1 {
        node-id 0;
        address ipv4 192.168.104.1:7008;
        volume 0 { device minor 1008; disk /dev/ambar/p8; meta-disk internal; }
    }
    on lima-dv-2 {
        node-id 1;
        address ipv4 192.168.104.3:7008;
        volume 0 { device minor 1008; disk /dev/ambar/p8; meta-disk internal; }
    }
    connection-mesh { hosts lima-dv-1 lima-dv-2; }
}
19:47:13.235
$ [dv-1] drbdadm adjust p8
rc=0
$ [dv-2] drbdadm adjust p8
rc=0
$ [dv-1] drbdsetup status p8 --verbose
p8 node-id:0 role:Primary suspended:no force-io-failures:no
  volume:0 minor:1008 disk:UpToDate backing_dev:/dev/ambar/p8 quorum:yes open:yes
      blocked:no
  lima-dv-2 node-id:1 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:8 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no

rc=0
$ [dv-2] drbdsetup status p8 --verbose
p8 node-id:1 role:Secondary suspended:no force-io-failures:no
  volume:0 minor:1008 disk:UpToDate backing_dev:/dev/ambar/p8 quorum:yes open:no
      blocked:no
  lima-dv-1 node-id:0 connection:Connected role:Primary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no

rc=0

### P8 step 3: writer log, kernel log, quorum
--- watch.sh tail
19:47:03.007 acked=1002 probe=alive:S
19:47:03.528 acked=1099 probe=alive:S
19:47:04.038 acked=1191 probe=alive:S
19:47:04.549 acked=1277 probe=alive:S
19:47:05.062 acked=1407 probe=alive:S
19:47:05.572 acked=1550 probe=alive:S
19:47:06.079 acked=1737 probe=alive:S
19:47:06.587 acked=1905 probe=alive:S
19:47:07.097 acked=2056 probe=alive:S
19:47:07.606 acked=2201 probe=alive:S
19:47:08.116 acked=2342 probe=alive:S
19:47:08.636 acked=2468 probe=alive:S
19:47:09.146 acked=2608 probe=alive:S
19:47:09.658 acked=2727 probe=alive:S
19:47:10.172 acked=2844 probe=alive:S
19:47:10.684 acked=2978 probe=alive:S
19:47:11.197 acked=3106 probe=alive:S
19:47:11.708 acked=3259 probe=alive:S
19:47:12.221 acked=3398 probe=alive:S
19:47:12.734 acked=3534 probe=alive:S
19:47:13.247 acked=3666 probe=alive:S
19:47:13.758 acked=3788 probe=alive:S
19:47:14.269 acked=3889 probe=alive:S
19:47:14.778 acked=4002 probe=alive:S
19:47:15.289 acked=4120 probe=alive:S
19:47:15.801 acked=4221 probe=alive:S
19:47:16.312 acked=4359 probe=alive:S
19:47:16.823 acked=4489 probe=alive:S
19:47:17.332 acked=4641 probe=alive:S
19:47:17.842 acked=4793 probe=alive:S
19:47:18.353 acked=4946 probe=alive:S
19:47:18.864 acked=5057 probe=alive:S
19:47:19.377 acked=5141 probe=alive:S
19:47:19.887 acked=5285 probe=alive:S
19:47:20.396 acked=5458 probe=alive:S
19:47:20.909 acked=5629 probe=alive:S
19:47:21.424 acked=5761 probe=alive:S
19:47:21.942 acked=5867 probe=alive:S
19:47:22.454 acked=5960 probe=alive:S
19:47:22.970 acked=6046 probe=alive:S
--- probe.sh log
--- dmesg
[Fri Nov 13 19:46:45 2026] drbd p8 lima-dv-2: conn( Connecting -> Connected ) peer( Unknown -> Secondary ) [connected]
[Fri Nov 13 19:46:45 2026] drbd p8/0 drbd1008: quorum( no -> yes ) [connected]
[Fri Nov 13 19:46:45 2026] drbd p8/0 drbd1008 lima-dv-2: pdsk( DUnknown -> Inconsistent ) repl( Off -> Established ) replication( yes -> no ) peer_replication( yes -> no ) [connected]
[Fri Nov 13 19:46:45 2026] drbd p8/0 drbd1008: Resumed AL updates
[Fri Nov 13 19:46:46 2026] drbd p8 lima-dv-3: Handshake to peer 2 successful: Agreed network protocol version 125
[Fri Nov 13 19:46:46 2026] drbd p8 lima-dv-3: Feature flags enabled on protocol level: 0x7ff TRIM THIN_RESYNC WRITE_SAME WRITE_ZEROES RESYNC_DAGTAG WRITE_POSTPONE
[Fri Nov 13 19:46:46 2026] drbd p8: Preparing remote state change 3590099414: 1->2 role( Secondary ) conn( Connected )
[Fri Nov 13 19:46:46 2026] drbd p8 lima-dv-2: Committing remote state change 3590099414 (primary_nodes=0)
[Fri Nov 13 19:46:46 2026] drbd p8: Preparing cluster-wide state change 3865360331: 0->2 role( Secondary ) conn( Connected )
[Fri Nov 13 19:46:46 2026] drbd p8/0 drbd1008 lima-dv-3: self 0000000000000004:0000000000000000:0000000000000000:0000000000000000 bits:0 flags:0
[Fri Nov 13 19:46:46 2026] drbd p8/0 drbd1008 lima-dv-3: peer's exposed UUID: 0000000000000000
[Fri Nov 13 19:46:46 2026] drbd p8: State change 3865360331: primary_nodes=0, weak_nodes=0
[Fri Nov 13 19:46:46 2026] drbd p8: Committing cluster-wide state change 3865360331 (16ms)
[Fri Nov 13 19:46:46 2026] drbd p8 lima-dv-3: conn( Connecting -> Connected ) peer( Unknown -> Secondary ) [connected]
[Fri Nov 13 19:46:46 2026] drbd p8/0 drbd1008 lima-dv-3: pdsk( DUnknown -> Diskless ) repl( Off -> Established ) peer_replication( yes -> no ) [connected]
[Fri Nov 13 19:46:52 2026] drbd p8/0 drbd1008 lima-dv-2: Preparing to skip initial sync
[Fri Nov 13 19:46:52 2026] drbd p8/0 drbd1008: Bitmap UUID for initial resync: 98134CC01C44E404
[Fri Nov 13 19:46:52 2026] drbd p8/0 drbd1008: new current UUID: 2DC8A9ED69CE5784 weak: FFFFFFFFFFFFFFFD
[Fri Nov 13 19:46:52 2026] drbd p8/0 drbd1008 lima-dv-3: cleared bitmap UUID 2DC8A9ED69CE5784:0000000000000000:0000000000000000:0000000000000000
[Fri Nov 13 19:46:52 2026] drbd p8/0 drbd1008 lima-dv-2: cleared bitmap UUID 2DC8A9ED69CE5784:0000000000000000:0000000000000000:0000000000000000
[Fri Nov 13 19:46:52 2026] drbd p8/0 drbd1008: disk( Inconsistent -> UpToDate ) [new-c-uuid]
[Fri Nov 13 19:46:52 2026] drbd p8/0 drbd1008 lima-dv-2: pdsk( Inconsistent -> UpToDate ) replication( no -> yes ) [new-c-uuid]
[Fri Nov 13 19:46:52 2026] drbd p8/0 drbd1008: persisting effective size = 1024 MB (1048316 KB)
[Fri Nov 13 19:46:57 2026] drbd p8: Preparing cluster-wide state change 1309512101: 0->all role( Primary )
[Fri Nov 13 19:46:57 2026] drbd p8: State change 1309512101: primary_nodes=1, weak_nodes=FFFFFFFFFFFFFFF8
[Fri Nov 13 19:46:57 2026] drbd p8: Committing cluster-wide state change 1309512101 (0ms)
[Fri Nov 13 19:46:57 2026] drbd p8: role( Secondary -> Primary ) [mkfs.ext4:158924 auto-promote]
[Fri Nov 13 19:46:57 2026] drbd p8/0 drbd1008: gen-obligation: NONE -> ARMED(promoted)
[Fri Nov 13 19:46:57 2026] drbd p8/0 drbd1008: gen-obligation: ARMED(promoted) -> MINTING(promoted)
[Fri Nov 13 19:46:57 2026] drbd p8/0 drbd1008: gen-obligation: MINTING(promoted) -> DISCHARGED(promoted)
[Fri Nov 13 19:46:57 2026] drbd p8: Preparing cluster-wide state change 4235780657: 0->all role( Secondary )
[Fri Nov 13 19:46:57 2026] drbd p8: State change 4235780657: primary_nodes=0, weak_nodes=0
[Fri Nov 13 19:46:57 2026] drbd p8: Committing cluster-wide state change 4235780657 (0ms)
[Fri Nov 13 19:46:57 2026] drbd p8: role( Primary -> Secondary ) [mkfs.ext4:158924 auto-demote]
[Fri Nov 13 19:46:57 2026] drbd p8: Preparing cluster-wide state change 806938607: 0->all role( Primary )
[Fri Nov 13 19:46:57 2026] drbd p8: State change 806938607: primary_nodes=1, weak_nodes=FFFFFFFFFFFFFFF8
[Fri Nov 13 19:46:57 2026] drbd p8: Committing cluster-wide state change 806938607 (0ms)
[Fri Nov 13 19:46:57 2026] drbd p8: role( Secondary -> Primary ) [mount:158954 auto-promote]
[Fri Nov 13 19:46:57 2026] drbd p8/0 drbd1008: gen-obligation: DISCHARGED(promoted) -> ARMED(promoted)
[Fri Nov 13 19:46:57 2026] drbd p8/0 drbd1008: gen-obligation: ARMED(promoted) -> MINTING(promoted)
[Fri Nov 13 19:46:57 2026] drbd p8/0 drbd1008: gen-obligation: MINTING(promoted) -> DISCHARGED(promoted)
[Fri Nov 13 19:47:03 2026] drbd p8: Preparing remote state change 750299192: 2->0 conn( Disconnecting ) disk( Outdated )
[Fri Nov 13 19:47:03 2026] drbd p8 lima-dv-3: Committing remote state change 750299192 (primary_nodes=1)
[Fri Nov 13 19:47:03 2026] drbd p8 lima-dv-3: conn( Connected -> TearDown ) peer( Secondary -> Unknown ) [remote]
[Fri Nov 13 19:47:03 2026] drbd p8/0 drbd1008 lima-dv-3: pdsk( Diskless -> DUnknown ) repl( Established -> Off ) [remote]
[Fri Nov 13 19:47:03 2026] drbd p8 lima-dv-3: Terminating sender thread
[Fri Nov 13 19:47:03 2026] drbd p8 lima-dv-3: Starting sender thread (peer-node-id 2)
[Fri Nov 13 19:47:03 2026] drbd p8 lima-dv-3: Connection closed
[Fri Nov 13 19:47:03 2026] drbd p8 lima-dv-3: conn( TearDown -> Unconnected ) [disconnected]
[Fri Nov 13 19:47:03 2026] drbd p8 lima-dv-3: Restarting receiver thread
[Fri Nov 13 19:47:03 2026] drbd p8 lima-dv-3: conn( Unconnected -> Connecting ) [connecting]
[Fri Nov 13 19:47:03 2026] drbd p8: Preparing remote state change 4192292609: 2->1 conn( Disconnecting )
[Fri Nov 13 19:47:03 2026] drbd p8 lima-dv-2: Committing remote state change 4192292609 (primary_nodes=1)
[Fri Nov 13 19:47:14 2026] drbd p8 lima-dv-3: conn( Connecting -> Disconnecting ) [del-peer]
[Fri Nov 13 19:47:14 2026] drbd p8 lima-dv-3: Terminating sender thread
[Fri Nov 13 19:47:14 2026] drbd p8 lima-dv-3: Starting sender thread (peer-node-id 2)
[Fri Nov 13 19:47:14 2026] drbd p8 lima-dv-3: Connection closed
[Fri Nov 13 19:47:14 2026] drbd p8 lima-dv-3: conn( Disconnecting -> StandAlone ) [disconnected]
[Fri Nov 13 19:47:14 2026] drbd p8 lima-dv-3: Terminating receiver thread
[Fri Nov 13 19:47:14 2026] drbd p8 lima-dv-3: Terminating sender thread
$ [dv-1] drbdsetup status p8 --verbose | grep -o 'quorum:[a-z]*'
quorum:yes
rc=0
$ [dv-2] drbdsetup status p8 --verbose | grep -o 'quorum:[a-z]*'
quorum:yes
rc=0

### P8 step 4: dv-3 returns with the same node-id
19:47:27.567
$ [dv-1] drbdadm adjust p8
rc=0
$ [dv-2] drbdadm adjust p8
rc=0
$ [dv-3] drbdadm up p8
rc=0
$ [dv-1] drbdsetup status p8 --verbose --statistics
p8 node-id:0 role:Primary suspended:no force-io-failures:no
    write-ordering:flush
  volume:0 minor:1008 disk:UpToDate backing_dev:/dev/ambar/p8 quorum:yes open:yes
      size:1048316 read:1005 written:1248108 al-writes:5 bm-writes:112 upper-pending:2
      lower-pending:0 al-suspended:no blocked:no
  lima-dv-2 node-id:1 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:16 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
        received:0 sent:166896 out-of-sync:0 pending:1 unacked:0
  lima-dv-3 node-id:2 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:Diskless peer-client:yes
        resync-suspended:no
        received:0 sent:0 out-of-sync:0 pending:0 unacked:0

rc=0
$ [dv-2] drbdsetup status p8 --verbose --statistics
p8 node-id:1 role:Secondary suspended:no force-io-failures:no
    write-ordering:flush
  volume:0 minor:1008 disk:UpToDate backing_dev:/dev/ambar/p8 quorum:yes open:no
      size:1048316 read:0 written:1251988 al-writes:5 bm-writes:112 upper-pending:0
      lower-pending:1 al-suspended:no blocked:no
  lima-dv-1 node-id:0 connection:Connected role:Primary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
        received:170780 sent:0 out-of-sync:0 pending:0 unacked:2
  lima-dv-3 node-id:2 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:Diskless peer-client:yes
        resync-suspended:no
        received:0 sent:0 out-of-sync:0 pending:0 unacked:0

rc=0
$ [dv-3] drbdsetup status p8 --verbose --statistics
p8 node-id:2 role:Secondary suspended:no force-io-failures:no
    write-ordering:none
  volume:0 minor:1008 disk:Diskless client:yes backing_dev:none quorum:yes open:no
      size:1048316 read:0 written:0 al-writes:0 bm-writes:0 upper-pending:0
      lower-pending:0 al-suspended:no blocked:no
  lima-dv-1 node-id:0 connection:Connected role:Primary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
        received:0 sent:0 out-of-sync:0 pending:0 unacked:0
  lima-dv-2 node-id:1 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
        received:0 sent:0 out-of-sync:0 pending:0 unacked:0

rc=0
--- watch.sh tail
19:47:33.237 acked=8274 probe=alive:S
19:47:33.746 acked=8434 probe=alive:S
19:47:34.254 acked=8607 probe=alive:S
19:47:34.763 acked=8753 probe=alive:S
19:47:35.275 acked=8894 probe=alive:S
19:47:35.784 acked=9024 probe=alive:S
19:47:36.297 acked=9120 probe=alive:S
19:47:36.807 acked=9238 probe=alive:S
19:47:37.315 acked=9373 probe=alive:S
19:47:37.825 acked=9531 probe=alive:S
19:47:38.337 acked=9708 probe=alive:S
19:47:38.845 acked=9887 probe=alive:S
19:47:39.354 acked=10042 probe=alive:S
19:47:39.878 acked=10155 probe=alive:S
19:47:40.401 acked=10282 probe=alive:S
19:47:40.912 acked=10411 probe=alive:S
19:47:41.422 acked=10532 probe=alive:S
19:47:41.936 acked=10621 probe=alive:S
19:47:42.448 acked=10712 probe=alive:S
19:47:42.964 acked=10813 probe=alive:S
--- dmesg
[Fri Nov 13 19:46:57 2026] drbd p8/0 drbd1008: gen-obligation: DISCHARGED(promoted) -> ARMED(promoted)
[Fri Nov 13 19:46:57 2026] drbd p8/0 drbd1008: gen-obligation: ARMED(promoted) -> MINTING(promoted)
[Fri Nov 13 19:46:57 2026] drbd p8/0 drbd1008: gen-obligation: MINTING(promoted) -> DISCHARGED(promoted)
[Fri Nov 13 19:47:03 2026] drbd p8: Preparing remote state change 750299192: 2->0 conn( Disconnecting ) disk( Outdated )
[Fri Nov 13 19:47:03 2026] drbd p8 lima-dv-3: Committing remote state change 750299192 (primary_nodes=1)
[Fri Nov 13 19:47:03 2026] drbd p8 lima-dv-3: conn( Connected -> TearDown ) peer( Secondary -> Unknown ) [remote]
[Fri Nov 13 19:47:03 2026] drbd p8/0 drbd1008 lima-dv-3: pdsk( Diskless -> DUnknown ) repl( Established -> Off ) [remote]
[Fri Nov 13 19:47:03 2026] drbd p8 lima-dv-3: Terminating sender thread
[Fri Nov 13 19:47:03 2026] drbd p8 lima-dv-3: Starting sender thread (peer-node-id 2)
[Fri Nov 13 19:47:03 2026] drbd p8 lima-dv-3: Connection closed
[Fri Nov 13 19:47:03 2026] drbd p8 lima-dv-3: conn( TearDown -> Unconnected ) [disconnected]
[Fri Nov 13 19:47:03 2026] drbd p8 lima-dv-3: Restarting receiver thread
[Fri Nov 13 19:47:03 2026] drbd p8 lima-dv-3: conn( Unconnected -> Connecting ) [connecting]
[Fri Nov 13 19:47:03 2026] drbd p8: Preparing remote state change 4192292609: 2->1 conn( Disconnecting )
[Fri Nov 13 19:47:03 2026] drbd p8 lima-dv-2: Committing remote state change 4192292609 (primary_nodes=1)
[Fri Nov 13 19:47:14 2026] drbd p8 lima-dv-3: conn( Connecting -> Disconnecting ) [del-peer]
[Fri Nov 13 19:47:14 2026] drbd p8 lima-dv-3: Terminating sender thread
[Fri Nov 13 19:47:14 2026] drbd p8 lima-dv-3: Starting sender thread (peer-node-id 2)
[Fri Nov 13 19:47:14 2026] drbd p8 lima-dv-3: Connection closed
[Fri Nov 13 19:47:14 2026] drbd p8 lima-dv-3: conn( Disconnecting -> StandAlone ) [disconnected]
[Fri Nov 13 19:47:14 2026] drbd p8 lima-dv-3: Terminating receiver thread
[Fri Nov 13 19:47:14 2026] drbd p8 lima-dv-3: Terminating sender thread
[Fri Nov 13 19:47:28 2026] drbd p8 lima-dv-3: Starting sender thread (peer-node-id 2)
[Fri Nov 13 19:47:28 2026] drbd p8 lima-dv-3: conn( StandAlone -> Unconnected ) [connect]
[Fri Nov 13 19:47:28 2026] drbd p8 lima-dv-3: Starting receiver thread (peer-node-id 2)
[Fri Nov 13 19:47:28 2026] drbd p8 lima-dv-3: conn( Unconnected -> Connecting ) [connecting]
[Fri Nov 13 19:47:31 2026] drbd p8 lima-dv-3: Handshake to peer 2 successful: Agreed network protocol version 125
[Fri Nov 13 19:47:31 2026] drbd p8 lima-dv-3: Feature flags enabled on protocol level: 0x7ff TRIM THIN_RESYNC WRITE_SAME WRITE_ZEROES RESYNC_DAGTAG WRITE_POSTPONE
[Fri Nov 13 19:47:31 2026] drbd p8: Preparing cluster-wide state change 1055869879: 0->2 role( Primary ) conn( Connected )
[Fri Nov 13 19:47:31 2026] drbd p8/0 drbd1008 lima-dv-3: self 2DC8A9ED69CE5784:0000000000000000:0000000000000000:0000000000000000 bits:0 flags:0
[Fri Nov 13 19:47:31 2026] drbd p8/0 drbd1008 lima-dv-3: peer's exposed UUID: 0000000000000000
[Fri Nov 13 19:47:31 2026] drbd p8: State change 1055869879: primary_nodes=1, weak_nodes=FFFFFFFFFFFFFFF8
[Fri Nov 13 19:47:31 2026] drbd p8: Committing cluster-wide state change 1055869879 (20ms)
[Fri Nov 13 19:47:31 2026] drbd p8 lima-dv-3: conn( Connecting -> Connected ) peer( Unknown -> Secondary ) [connected]
[Fri Nov 13 19:47:31 2026] drbd p8/0 drbd1008 lima-dv-3: pdsk( DUnknown -> Diskless ) repl( Off -> Established ) [connected]
[Fri Nov 13 19:47:31 2026] drbd p8/0 drbd1008: gen-obligation: DISCHARGED(promoted) -> ARMED(peer-returned-diskless)
[Fri Nov 13 19:47:31 2026] drbd p8: Preparing remote state change 3548478819: 1->2 role( Secondary ) conn( Connected )
[Fri Nov 13 19:47:31 2026] drbd p8/0 drbd1008: gen-obligation: ARMED(peer-returned-diskless) -> MINTING(peer-returned-diskless)
[Fri Nov 13 19:47:31 2026] drbd p8/0 drbd1008: gen-obligation: MINTING(peer-returned-diskless) -> DISCHARGED(peer-returned-diskless)
[Fri Nov 13 19:47:31 2026] drbd p8 lima-dv-2: Committing remote state change 3548478819 (primary_nodes=1)

### P8 step 5: stop the writer, compare acknowledged and on-volume numbers
acked=10875 on-volume=10876
```

### P9. Признак файловой системы и занятое устройство

Ресурс `p9`: три копии, `create-md --max-peers=7`, `new-current-uuid --clear-bitmap`. Чтение с `dv-2` (роль `Secondary`), `mkfs` и монтирование на `dv-1`.

```bash
stand/spike/pre-csi.sh p9
```

```text

### P9: blkid -p and a 2-byte read at offset 1080 on /dev/drbd1009, read on dv-2
$ [dv-1] drbdadm create-md --force --max-peers=7 p9
initializing bitmap (224 KB) to all zero
initializing activity log
Writing meta data...
New drbd meta data block successfully created.
rc=0
$ [dv-1] drbdadm up p9
rc=0
$ [dv-2] drbdadm create-md --force --max-peers=7 p9
initializing bitmap (224 KB) to all zero
initializing activity log
Writing meta data...
New drbd meta data block successfully created.
rc=0
$ [dv-2] drbdadm up p9
rc=0
$ [dv-3] drbdadm create-md --force --max-peers=7 p9
initializing bitmap (224 KB) to all zero
initializing activity log
Writing meta data...
New drbd meta data block successfully created.
rc=0
$ [dv-3] drbdadm up p9
rc=0
$ [dv-1] drbdadm new-current-uuid --clear-bitmap p9/0
rc=0
$ [dv-1] drbdsetup status p9 --verbose
p9 node-id:0 role:Secondary suspended:no force-io-failures:no
  volume:0 minor:1009 disk:UpToDate backing_dev:/dev/ambar/p9 quorum:yes open:no
      blocked:no
  lima-dv-2 node-id:1 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
  lima-dv-3 node-id:2 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no

rc=0

### P9 (1): no filesystem
$ [dv-2] blkid -p -o value -s TYPE /dev/drbd1009
rc=2
--- stdout (first 20 lines)
--- stderr
$ [dv-2] dd if=/dev/drbd1009 bs=1 skip=1080 count=2 status=none | od -An -tx1
rc=0
--- stdout (first 20 lines)
 00 00
--- stderr

### P9 (2): after mkfs.ext4 -q on dv-1, not mounted
$ [dv-1] mkfs.ext4 -q /dev/drbd1009
rc=0
$ [dv-2] blkid -p -o value -s TYPE /dev/drbd1009
rc=0
--- stdout (first 20 lines)
ext4
--- stderr
$ [dv-2] dd if=/dev/drbd1009 bs=1 skip=1080 count=2 status=none | od -An -tx1
rc=0
--- stdout (first 20 lines)
 53 ef
--- stderr

### P9 (3): mounted on dv-1
$ [dv-1] drbdsetup status p9 --verbose
p9 node-id:0 role:Primary suspended:no force-io-failures:no
  volume:0 minor:1009 disk:UpToDate backing_dev:/dev/ambar/p9 quorum:yes open:yes
      blocked:no
  lima-dv-2 node-id:1 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
  lima-dv-3 node-id:2 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no

rc=0
$ [dv-2] blkid -p -o value -s TYPE /dev/drbd1009
rc=2
--- stdout (first 20 lines)
--- stderr
blkid: error: /dev/drbd1009: Wrong medium type
$ [dv-2] dd if=/dev/drbd1009 bs=1 skip=1080 count=2 status=none | od -An -tx1
rc=1
--- stdout (first 20 lines)
--- stderr
dd: failed to open '/dev/drbd1009': Wrong medium type
$ [dv-2] drbdsetup status p9 --verbose
p9 node-id:1 role:Secondary suspended:no force-io-failures:no
  volume:0 minor:1009 disk:UpToDate backing_dev:/dev/ambar/p9 quorum:yes open:no
      blocked:no
  lima-dv-1 node-id:0 connection:Connected role:Primary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
  lima-dv-3 node-id:2 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no

rc=0

### P9 (4): /dev/drbd1999 does not exist
$ [dv-2] ls -l /dev/drbd1999
rc=2
--- stdout (first 20 lines)
--- stderr
ls: cannot access '/dev/drbd1999': No such file or directory
$ [dv-2] blkid -p -o value -s TYPE /dev/drbd1999
rc=2
--- stdout (first 20 lines)
--- stderr
$ [dv-2] dd if=/dev/drbd1999 bs=1 skip=1080 count=2 status=none | od -An -tx1
rc=1
--- stdout (first 20 lines)
--- stderr
dd: failed to open '/dev/drbd1999': No such file or directory
```

### P10. Метаданные DRBD в шести состояниях

Ресурс `p10`: `dv-1` (`node-id` 0), `dv-2` (1), `dv-3` (2), у всех копия `/dev/ambar/p10` на 1 ГиБ, `meta-disk internal`, minor 1010, порт 7010, `create-md --force --max-peers=7` на всех трёх. Дамп снимается на `dv-1` при остановленном ресурсе; `dv-2` и `dv-3` в опыте остаются поднятыми, когда это указано в состоянии. В состояниях (2)–(6) дамп после `down` отвечает кодом 255, поэтому скрипт повторяет его через 5 с и затем после `drbdadm apply-al p10`; в таблице и ниже значения полей взяты из последнего успешного дампа. Время в заголовках дампов — по часам `dv-1`.

```bash
stand/spike/pre-csi.sh p10
```

Прогон 1, вывод целиком:

```text

### P10: full drbdadm dump-md of dv-1 in six states, resource p10 on dv-1, dv-2, dv-3
$ [dv-1] drbdadm create-md --force --max-peers=7 p10
initializing bitmap (224 KB) to all zero
initializing activity log
Writing meta data...
New drbd meta data block successfully created.
rc=0
$ [dv-2] drbdadm create-md --force --max-peers=7 p10
initializing bitmap (224 KB) to all zero
initializing activity log
Writing meta data...
New drbd meta data block successfully created.
rc=0
$ [dv-3] drbdadm create-md --force --max-peers=7 p10
initializing bitmap (224 KB) to all zero
initializing activity log
Writing meta data...
New drbd meta data block successfully created.
rc=0

### P10 (1): right after create-md --force --max-peers=7, never up
$ [dv-1] drbdadm dump-md p10
rc=0
--- stdout
# DRBD meta data dump
# 2026-11-13 12:45:16 +0000 [1794573916]
# lima-dv-1> drbdmeta 1010 v09 /dev/ambar/p10 internal dump-md
#

version "v09";

max-peers 7;
# md_size_sect 520
# md_offset 1073737728
# al_offset 1073704960
# bm_offset 1073475584

node-id -1;
current-uuid 0x0000000000000004;
flags 0x00000080;
members 0x0000000000000000;
features 0x0000000000000001;
peer[0] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[1] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[2] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[3] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[4] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[5] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[6] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[7] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[8] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[9] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[10] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[11] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[12] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[13] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[14] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[15] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[16] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[17] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[18] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[19] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[20] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[21] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[22] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[23] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[24] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[25] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[26] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[27] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[28] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[29] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[30] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[31] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
history-uuids {
        0x0000000000000000; 0x0000000000000000; 0x0000000000000000; 0x0000000000000000;
        0x0000000000000000; 0x0000000000000000; 0x0000000000000000; 0x0000000000000000;
        0x0000000000000000; 0x0000000000000000; 0x0000000000000000; 0x0000000000000000;
        0x0000000000000000; 0x0000000000000000; 0x0000000000000000; 0x0000000000000000;
        0x0000000000000000; 0x0000000000000000; 0x0000000000000000; 0x0000000000000000;
        0x0000000000000000; 0x0000000000000000; 0x0000000000000000; 0x0000000000000000;
        0x0000000000000000; 0x0000000000000000; 0x0000000000000000; 0x0000000000000000;
        0x0000000000000000; 0x0000000000000000; 0x0000000000000000; 0x0000000000000000;
}
# al-extents 257;
la-size-sect 0;
bm-byte-per-bit 4096;
device-uuid 0x9A9C813A219E8F2D;
la-peer-max-bio-size 0;
al-stripes 1;
al-stripe-size-4k 8;
# bm-bytes 0;
bitmap[0] {
}
# bits-set[0] 0;
bitmap[1] {
}
# bits-set[1] 0;
bitmap[2] {
}
# bits-set[2] 0;
bitmap[3] {
}
# bits-set[3] 0;
bitmap[4] {
}
# bits-set[4] 0;
bitmap[5] {
}
# bits-set[5] 0;
bitmap[6] {
}
# bits-set[6] 0;
--- stderr

### P10 (2): after up and down on dv-1 alone, dv-2 and dv-3 never configured
$ [dv-1] drbdadm up p10
rc=0
$ [dv-1] drbdsetup status p10 --verbose
p10 node-id:0 role:Secondary suspended:no force-io-failures:no
  volume:0 minor:1010 disk:Inconsistent backing_dev:/dev/ambar/p10 quorum:no
      open:no blocked:no
  lima-dv-2 node-id:1 connection:Connecting role:Unknown tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Off peer-disk:DUnknown resync-suspended:no
  lima-dv-3 node-id:2 connection:Connecting role:Unknown tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Off peer-disk:DUnknown resync-suspended:no

rc=0
$ [dv-1] drbdadm down p10
rc=0
$ [dv-1] drbdadm dump-md p10
rc=255
--- stdout
--- stderr
Found meta data is "unclean", please apply-al first
Command 'drbdmeta 1010 v09 /dev/ambar/p10 internal dump-md' terminated with exit code 255
--- again after 5 s
$ [dv-1] drbdadm dump-md p10
rc=255
--- stdout
--- stderr
Found meta data is "unclean", please apply-al first
Command 'drbdmeta 1010 v09 /dev/ambar/p10 internal dump-md' terminated with exit code 255
$ [dv-1] drbdadm apply-al p10
rc=0
--- again after apply-al
$ [dv-1] drbdadm dump-md p10
rc=0
--- stdout
# DRBD meta data dump
# 2026-11-13 12:45:31 +0000 [1794573931]
# lima-dv-1> drbdmeta 1010 v09 /dev/ambar/p10 internal dump-md
#

version "v09";

max-peers 7;
# md_size_sect 520
# md_offset 1073737728
# al_offset 1073704960
# bm_offset 1073475584

node-id 0;
current-uuid 0x0000000000000004;
flags 0x00000480;
members 0x0000000000000001;
features 0x0000000000000001;
peer[0] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[1] {
    bitmap-index 1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[2] {
    bitmap-index 0;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[3] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[4] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[5] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[6] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[7] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[8] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[9] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[10] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[11] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[12] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[13] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[14] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[15] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[16] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[17] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[18] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[19] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[20] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[21] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[22] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[23] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[24] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[25] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[26] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[27] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[28] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[29] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[30] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[31] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
history-uuids {
        0x0000000000000000; 0x0000000000000000; 0x0000000000000000; 0x0000000000000000;
        0x0000000000000000; 0x0000000000000000; 0x0000000000000000; 0x0000000000000000;
        0x0000000000000000; 0x0000000000000000; 0x0000000000000000; 0x0000000000000000;
        0x0000000000000000; 0x0000000000000000; 0x0000000000000000; 0x0000000000000000;
        0x0000000000000000; 0x0000000000000000; 0x0000000000000000; 0x0000000000000000;
        0x0000000000000000; 0x0000000000000000; 0x0000000000000000; 0x0000000000000000;
        0x0000000000000000; 0x0000000000000000; 0x0000000000000000; 0x0000000000000000;
        0x0000000000000000; 0x0000000000000000; 0x0000000000000000; 0x0000000000000000;
}
# al-extents 1237;
la-size-sect 0;
bm-byte-per-bit 4096;
device-uuid 0x9A9C813A219E8F2D;
la-peer-max-bio-size 1048576;
al-stripes 1;
al-stripe-size-4k 8;
# bm-bytes 0;
bitmap[0] {
}
# bits-set[0] 0;
bitmap[1] {
}
# bits-set[1] 0;
bitmap[2] {
}
# bits-set[2] 0;
bitmap[3] {
}
# bits-set[3] 0;
bitmap[4] {
}
# bits-set[4] 0;
bitmap[5] {
}
# bits-set[5] 0;
bitmap[6] {
}
# bits-set[6] 0;
--- stderr

### P10 (3): all three up and connected, disks Inconsistent, no initial-sync skip; then down on dv-1
$ [dv-1] drbdadm up p10
rc=0
$ [dv-2] drbdadm up p10
rc=0
$ [dv-3] drbdadm up p10
rc=0
wait: dv-1: two connections Connected after 2 s at most
wait: dv-2: two connections Connected after 2 s at most
wait: dv-3: two connections Connected after 2 s at most
$ [dv-1] drbdsetup status p10 --verbose
p10 node-id:0 role:Secondary suspended:no force-io-failures:no
  volume:0 minor:1010 disk:Inconsistent backing_dev:/dev/ambar/p10 quorum:yes
      open:no blocked:no
  lima-dv-2 node-id:1 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:Inconsistent resync-suspended:no
  lima-dv-3 node-id:2 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:Inconsistent resync-suspended:no

rc=0
$ [dv-2] drbdsetup status p10 --verbose
p10 node-id:1 role:Secondary suspended:no force-io-failures:no
  volume:0 minor:1010 disk:Inconsistent backing_dev:/dev/ambar/p10 quorum:yes
      open:no blocked:no
  lima-dv-1 node-id:0 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:Inconsistent resync-suspended:no
  lima-dv-3 node-id:2 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:Inconsistent resync-suspended:no

rc=0
$ [dv-3] drbdsetup status p10 --verbose
p10 node-id:2 role:Secondary suspended:no force-io-failures:no
  volume:0 minor:1010 disk:Inconsistent backing_dev:/dev/ambar/p10 quorum:yes
      open:no blocked:no
  lima-dv-1 node-id:0 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:Inconsistent resync-suspended:no
  lima-dv-2 node-id:1 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:Inconsistent resync-suspended:no

rc=0
$ [dv-1] drbdadm down p10
rc=0
$ [dv-1] drbdadm dump-md p10
rc=255
--- stdout
--- stderr
Found meta data is "unclean", please apply-al first
Command 'drbdmeta 1010 v09 /dev/ambar/p10 internal dump-md' terminated with exit code 255
--- again after 5 s
$ [dv-1] drbdadm dump-md p10
rc=255
--- stdout
--- stderr
Found meta data is "unclean", please apply-al first
Command 'drbdmeta 1010 v09 /dev/ambar/p10 internal dump-md' terminated with exit code 255
$ [dv-1] drbdadm apply-al p10
rc=0
--- again after apply-al
$ [dv-1] drbdadm dump-md p10
rc=0
--- stdout
# DRBD meta data dump
# 2026-11-13 12:45:52 +0000 [1794573952]
# lima-dv-1> drbdmeta 1010 v09 /dev/ambar/p10 internal dump-md
#

version "v09";

max-peers 7;
# md_size_sect 520
# md_offset 1073737728
# al_offset 1073704960
# bm_offset 1073475584

node-id 0;
current-uuid 0x0000000000000004;
flags 0x00000480;
members 0x0000000000000001;
features 0x0000000000000001;
peer[0] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00010000; # exists
}
peer[1] {
    bitmap-index 1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000052; # <=outdated | seen | 0x40=?
}
peer[2] {
    bitmap-index 0;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000052; # <=outdated | seen | 0x40=?
}
peer[3] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[4] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[5] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[6] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[7] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[8] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[9] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[10] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[11] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[12] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[13] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[14] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[15] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[16] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[17] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[18] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[19] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[20] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[21] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[22] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[23] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[24] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[25] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[26] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[27] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[28] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[29] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[30] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[31] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
history-uuids {
        0x0000000000000000; 0x0000000000000000; 0x0000000000000000; 0x0000000000000000;
        0x0000000000000000; 0x0000000000000000; 0x0000000000000000; 0x0000000000000000;
        0x0000000000000000; 0x0000000000000000; 0x0000000000000000; 0x0000000000000000;
        0x0000000000000000; 0x0000000000000000; 0x0000000000000000; 0x0000000000000000;
        0x0000000000000000; 0x0000000000000000; 0x0000000000000000; 0x0000000000000000;
        0x0000000000000000; 0x0000000000000000; 0x0000000000000000; 0x0000000000000000;
        0x0000000000000000; 0x0000000000000000; 0x0000000000000000; 0x0000000000000000;
        0x0000000000000000; 0x0000000000000000; 0x0000000000000000; 0x0000000000000000;
}
# al-extents 1237;
la-size-sect 0;
bm-byte-per-bit 4096;
device-uuid 0x9A9C813A219E8F2D;
la-peer-max-bio-size 1048576;
al-stripes 1;
al-stripe-size-4k 8;
# bm-bytes 0;
bitmap[0] {
}
# bits-set[0] 0;
bitmap[1] {
}
# bits-set[1] 0;
bitmap[2] {
}
# bits-set[2] 0;
bitmap[3] {
}
# bits-set[3] 0;
bitmap[4] {
}
# bits-set[4] 0;
bitmap[5] {
}
# bits-set[5] 0;
bitmap[6] {
}
# bits-set[6] 0;
--- stderr

### P10 (4): dv-1 up again, new-current-uuid --clear-bitmap on dv-1, all UpToDate; then down on dv-1
$ [dv-1] drbdadm up p10
rc=0
wait: dv-1: two connections Connected after 2 s at most
$ [dv-1] drbdadm new-current-uuid --clear-bitmap p10/0
rc=0
wait: dv-1: own disk UpToDate, two peers UpToDate after 2 s at most
wait: dv-2: own disk UpToDate, two peers UpToDate after 2 s at most
wait: dv-3: own disk UpToDate, two peers UpToDate after 2 s at most
$ [dv-1] drbdsetup status p10 --verbose
p10 node-id:0 role:Secondary suspended:no force-io-failures:no
  volume:0 minor:1010 disk:UpToDate backing_dev:/dev/ambar/p10 quorum:yes open:no
      blocked:no
  lima-dv-2 node-id:1 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
  lima-dv-3 node-id:2 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no

rc=0
$ [dv-2] drbdsetup status p10 --verbose
p10 node-id:1 role:Secondary suspended:no force-io-failures:no
  volume:0 minor:1010 disk:UpToDate backing_dev:/dev/ambar/p10 quorum:yes open:no
      blocked:no
  lima-dv-1 node-id:0 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
  lima-dv-3 node-id:2 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no

rc=0
$ [dv-3] drbdsetup status p10 --verbose
p10 node-id:2 role:Secondary suspended:no force-io-failures:no
  volume:0 minor:1010 disk:UpToDate backing_dev:/dev/ambar/p10 quorum:yes open:no
      blocked:no
  lima-dv-1 node-id:0 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
  lima-dv-2 node-id:1 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no

rc=0
$ [dv-1] drbdadm down p10
rc=0
$ [dv-1] drbdadm dump-md p10
rc=255
--- stdout
--- stderr
Found meta data is "unclean", please apply-al first
Command 'drbdmeta 1010 v09 /dev/ambar/p10 internal dump-md' terminated with exit code 255
--- again after 5 s
$ [dv-1] drbdadm dump-md p10
rc=255
--- stdout
--- stderr
Found meta data is "unclean", please apply-al first
Command 'drbdmeta 1010 v09 /dev/ambar/p10 internal dump-md' terminated with exit code 255
$ [dv-1] drbdadm apply-al p10
rc=0
--- again after apply-al
$ [dv-1] drbdadm dump-md p10
rc=0
--- stdout
# DRBD meta data dump
# 2026-11-13 12:46:14 +0000 [1794573974]
# lima-dv-1> drbdmeta 1010 v09 /dev/ambar/p10 internal dump-md
#

version "v09";

max-peers 7;
# md_size_sect 520
# md_offset 1073737728
# al_offset 1073704960
# bm_offset 1073475584

node-id 0;
current-uuid 0x0240DDD21A4B2CE8;
flags 0x00000091;
members 0x0000000000000001;
features 0x0000000000000001;
peer[0] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00010000; # exists
}
peer[1] {
    bitmap-index 1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000010; # seen
}
peer[2] {
    bitmap-index 0;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000010; # seen
}
peer[3] {
    bitmap-index -1;
    bitmap-uuid 0x5400D259A0DF9D13;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[4] {
    bitmap-index -1;
    bitmap-uuid 0x5400D259A0DF9D13;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[5] {
    bitmap-index -1;
    bitmap-uuid 0x5400D259A0DF9D13;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[6] {
    bitmap-index -1;
    bitmap-uuid 0x5400D259A0DF9D13;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[7] {
    bitmap-index -1;
    bitmap-uuid 0x5400D259A0DF9D13;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[8] {
    bitmap-index -1;
    bitmap-uuid 0x5400D259A0DF9D13;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[9] {
    bitmap-index -1;
    bitmap-uuid 0x5400D259A0DF9D13;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[10] {
    bitmap-index -1;
    bitmap-uuid 0x5400D259A0DF9D13;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[11] {
    bitmap-index -1;
    bitmap-uuid 0x5400D259A0DF9D13;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[12] {
    bitmap-index -1;
    bitmap-uuid 0x5400D259A0DF9D13;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[13] {
    bitmap-index -1;
    bitmap-uuid 0x5400D259A0DF9D13;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[14] {
    bitmap-index -1;
    bitmap-uuid 0x5400D259A0DF9D13;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[15] {
    bitmap-index -1;
    bitmap-uuid 0x5400D259A0DF9D13;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[16] {
    bitmap-index -1;
    bitmap-uuid 0x5400D259A0DF9D13;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[17] {
    bitmap-index -1;
    bitmap-uuid 0x5400D259A0DF9D13;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[18] {
    bitmap-index -1;
    bitmap-uuid 0x5400D259A0DF9D13;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[19] {
    bitmap-index -1;
    bitmap-uuid 0x5400D259A0DF9D13;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[20] {
    bitmap-index -1;
    bitmap-uuid 0x5400D259A0DF9D13;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[21] {
    bitmap-index -1;
    bitmap-uuid 0x5400D259A0DF9D13;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[22] {
    bitmap-index -1;
    bitmap-uuid 0x5400D259A0DF9D13;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[23] {
    bitmap-index -1;
    bitmap-uuid 0x5400D259A0DF9D13;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[24] {
    bitmap-index -1;
    bitmap-uuid 0x5400D259A0DF9D13;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[25] {
    bitmap-index -1;
    bitmap-uuid 0x5400D259A0DF9D13;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[26] {
    bitmap-index -1;
    bitmap-uuid 0x5400D259A0DF9D13;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[27] {
    bitmap-index -1;
    bitmap-uuid 0x5400D259A0DF9D13;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[28] {
    bitmap-index -1;
    bitmap-uuid 0x5400D259A0DF9D13;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[29] {
    bitmap-index -1;
    bitmap-uuid 0x5400D259A0DF9D13;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[30] {
    bitmap-index -1;
    bitmap-uuid 0x5400D259A0DF9D13;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[31] {
    bitmap-index -1;
    bitmap-uuid 0x5400D259A0DF9D13;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
history-uuids {
        0x0000000000000000; 0x0000000000000000; 0x0000000000000000; 0x0000000000000000;
        0x0000000000000000; 0x0000000000000000; 0x0000000000000000; 0x0000000000000000;
        0x0000000000000000; 0x0000000000000000; 0x0000000000000000; 0x0000000000000000;
        0x0000000000000000; 0x0000000000000000; 0x0000000000000000; 0x0000000000000000;
        0x0000000000000000; 0x0000000000000000; 0x0000000000000000; 0x0000000000000000;
        0x0000000000000000; 0x0000000000000000; 0x0000000000000000; 0x0000000000000000;
        0x0000000000000000; 0x0000000000000000; 0x0000000000000000; 0x0000000000000000;
        0x0000000000000000; 0x0000000000000000; 0x0000000000000000; 0x0000000000000000;
}
# al-extents 1237;
la-size-sect 2096632;
bm-byte-per-bit 4096;
device-uuid 0x9A9C813A219E8F2D;
la-peer-max-bio-size 1048576;
al-stripes 1;
al-stripe-size-4k 8;
# bm-bytes 229376;
bitmap[0] {
   # at 0kB
    4096 times 0x0000000000000000;
}
# bits-set[0] 0;
bitmap[1] {
   # at 0kB
    4096 times 0x0000000000000000;
}
# bits-set[1] 0;
bitmap[2] {
   # at 0kB
    4096 times 0x0000000000000000;
}
# bits-set[2] 0;
bitmap[3] {
   # at 0kB
    4096 times 0x0000000000000000;
}
# bits-set[3] 0;
bitmap[4] {
   # at 0kB
    4096 times 0x0000000000000000;
}
# bits-set[4] 0;
bitmap[5] {
   # at 0kB
    4096 times 0x0000000000000000;
}
# bits-set[5] 0;
bitmap[6] {
   # at 0kB
    4096 times 0x0000000000000000;
}
# bits-set[6] 0;
--- stderr

### P10 (5): dv-1 up again, mkfs.ext4 and a file on dv-2, all UpToDate; then down on dv-1
$ [dv-1] drbdadm up p10
rc=0
wait: dv-1: own disk UpToDate, two peers UpToDate after 2 s at most
wait: dv-2: own disk UpToDate, two peers UpToDate after 2 s at most
wait: dv-3: own disk UpToDate, two peers UpToDate after 2 s at most
$ [dv-2] mkfs.ext4 -q /dev/drbd1010
rc=0
wait: dv-1: own disk UpToDate, two peers UpToDate after 2 s at most
wait: dv-2: own disk UpToDate, two peers UpToDate after 2 s at most
wait: dv-3: own disk UpToDate, two peers UpToDate after 2 s at most
$ [dv-1] drbdsetup status p10 --verbose
p10 node-id:0 role:Secondary suspended:no force-io-failures:no
  volume:0 minor:1010 disk:UpToDate backing_dev:/dev/ambar/p10 quorum:yes open:no
      blocked:no
  lima-dv-2 node-id:1 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
  lima-dv-3 node-id:2 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no

rc=0
$ [dv-2] drbdsetup status p10 --verbose
p10 node-id:1 role:Secondary suspended:no force-io-failures:no
  volume:0 minor:1010 disk:UpToDate backing_dev:/dev/ambar/p10 quorum:yes open:no
      blocked:no
  lima-dv-1 node-id:0 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
  lima-dv-3 node-id:2 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no

rc=0
$ [dv-3] drbdsetup status p10 --verbose
p10 node-id:2 role:Secondary suspended:no force-io-failures:no
  volume:0 minor:1010 disk:UpToDate backing_dev:/dev/ambar/p10 quorum:yes open:no
      blocked:no
  lima-dv-1 node-id:0 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
  lima-dv-2 node-id:1 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no

rc=0
$ [dv-1] drbdadm down p10
rc=0
$ [dv-1] drbdadm dump-md p10
rc=255
--- stdout
--- stderr
Found meta data is "unclean", please apply-al first
Command 'drbdmeta 1010 v09 /dev/ambar/p10 internal dump-md' terminated with exit code 255
--- again after 5 s
$ [dv-1] drbdadm dump-md p10
rc=255
--- stdout
--- stderr
Found meta data is "unclean", please apply-al first
Command 'drbdmeta 1010 v09 /dev/ambar/p10 internal dump-md' terminated with exit code 255
$ [dv-1] drbdadm apply-al p10
rc=0
--- again after apply-al
$ [dv-1] drbdadm dump-md p10
rc=0
--- stdout
# DRBD meta data dump
# 2026-11-13 12:46:37 +0000 [1794573997]
# lima-dv-1> drbdmeta 1010 v09 /dev/ambar/p10 internal dump-md
#

version "v09";

max-peers 7;
# md_size_sect 520
# md_offset 1073737728
# al_offset 1073704960
# bm_offset 1073475584

node-id 0;
current-uuid 0x5E59F70730D0DDAC;
flags 0x00000091;
members 0x0000000000000001;
features 0x0000000000000001;
peer[0] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00010000; # exists
}
peer[1] {
    bitmap-index 1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000010; # seen
}
peer[2] {
    bitmap-index 0;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000010; # seen
}
peer[3] {
    bitmap-index -1;
    bitmap-uuid 0x5400D259A0DF9D13;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[4] {
    bitmap-index -1;
    bitmap-uuid 0x5400D259A0DF9D13;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[5] {
    bitmap-index -1;
    bitmap-uuid 0x5400D259A0DF9D13;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[6] {
    bitmap-index -1;
    bitmap-uuid 0x5400D259A0DF9D13;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[7] {
    bitmap-index -1;
    bitmap-uuid 0x5400D259A0DF9D13;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[8] {
    bitmap-index -1;
    bitmap-uuid 0x5400D259A0DF9D13;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[9] {
    bitmap-index -1;
    bitmap-uuid 0x5400D259A0DF9D13;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[10] {
    bitmap-index -1;
    bitmap-uuid 0x5400D259A0DF9D13;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[11] {
    bitmap-index -1;
    bitmap-uuid 0x5400D259A0DF9D13;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[12] {
    bitmap-index -1;
    bitmap-uuid 0x5400D259A0DF9D13;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[13] {
    bitmap-index -1;
    bitmap-uuid 0x5400D259A0DF9D13;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[14] {
    bitmap-index -1;
    bitmap-uuid 0x5400D259A0DF9D13;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[15] {
    bitmap-index -1;
    bitmap-uuid 0x5400D259A0DF9D13;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[16] {
    bitmap-index -1;
    bitmap-uuid 0x5400D259A0DF9D13;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[17] {
    bitmap-index -1;
    bitmap-uuid 0x5400D259A0DF9D13;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[18] {
    bitmap-index -1;
    bitmap-uuid 0x5400D259A0DF9D13;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[19] {
    bitmap-index -1;
    bitmap-uuid 0x5400D259A0DF9D13;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[20] {
    bitmap-index -1;
    bitmap-uuid 0x5400D259A0DF9D13;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[21] {
    bitmap-index -1;
    bitmap-uuid 0x5400D259A0DF9D13;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[22] {
    bitmap-index -1;
    bitmap-uuid 0x5400D259A0DF9D13;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[23] {
    bitmap-index -1;
    bitmap-uuid 0x5400D259A0DF9D13;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[24] {
    bitmap-index -1;
    bitmap-uuid 0x5400D259A0DF9D13;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[25] {
    bitmap-index -1;
    bitmap-uuid 0x5400D259A0DF9D13;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[26] {
    bitmap-index -1;
    bitmap-uuid 0x5400D259A0DF9D13;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[27] {
    bitmap-index -1;
    bitmap-uuid 0x5400D259A0DF9D13;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[28] {
    bitmap-index -1;
    bitmap-uuid 0x5400D259A0DF9D13;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[29] {
    bitmap-index -1;
    bitmap-uuid 0x5400D259A0DF9D13;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[30] {
    bitmap-index -1;
    bitmap-uuid 0x5400D259A0DF9D13;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[31] {
    bitmap-index -1;
    bitmap-uuid 0x5400D259A0DF9D13;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
history-uuids {
        0x0240DDD21A4B2CE8; 0x0000000000000000; 0x0000000000000000; 0x0000000000000000;
        0x0000000000000000; 0x0000000000000000; 0x0000000000000000; 0x0000000000000000;
        0x0000000000000000; 0x0000000000000000; 0x0000000000000000; 0x0000000000000000;
        0x0000000000000000; 0x0000000000000000; 0x0000000000000000; 0x0000000000000000;
        0x0000000000000000; 0x0000000000000000; 0x0000000000000000; 0x0000000000000000;
        0x0000000000000000; 0x0000000000000000; 0x0000000000000000; 0x0000000000000000;
        0x0000000000000000; 0x0000000000000000; 0x0000000000000000; 0x0000000000000000;
        0x0000000000000000; 0x0000000000000000; 0x0000000000000000; 0x0000000000000000;
}
# al-extents 1237;
la-size-sect 2096632;
bm-byte-per-bit 4096;
device-uuid 0x9A9C813A219E8F2D;
la-peer-max-bio-size 1048576;
al-stripes 1;
al-stripe-size-4k 8;
# bm-bytes 229376;
bitmap[0] {
   # at 0kB
    4096 times 0x0000000000000000;
}
# bits-set[0] 0;
bitmap[1] {
   # at 0kB
    4096 times 0x0000000000000000;
}
# bits-set[1] 0;
bitmap[2] {
   # at 0kB
    4092 times 0xFFFFFFFFFFFFFFFF;
    0xFFFFFFFFFFFFFFFF; 0xFFFFFFFFFFFFFFFF; 0x7FFFFFFFFFFFFFFF; 0x0000000000000000;
}
# bits-set[2] 262079;
bitmap[3] {
   # at 0kB
    4092 times 0xFFFFFFFFFFFFFFFF;
    0xFFFFFFFFFFFFFFFF; 0xFFFFFFFFFFFFFFFF; 0x7FFFFFFFFFFFFFFF; 0x0000000000000000;
}
# bits-set[3] 262079;
bitmap[4] {
   # at 0kB
    4092 times 0xFFFFFFFFFFFFFFFF;
    0xFFFFFFFFFFFFFFFF; 0xFFFFFFFFFFFFFFFF; 0x7FFFFFFFFFFFFFFF; 0x0000000000000000;
}
# bits-set[4] 262079;
bitmap[5] {
   # at 0kB
    4092 times 0xFFFFFFFFFFFFFFFF;
    0xFFFFFFFFFFFFFFFF; 0xFFFFFFFFFFFFFFFF; 0x7FFFFFFFFFFFFFFF; 0x0000000000000000;
}
# bits-set[5] 262079;
bitmap[6] {
   # at 0kB
    4092 times 0xFFFFFFFFFFFFFFFF;
    0xFFFFFFFFFFFFFFFF; 0xFFFFFFFFFFFFFFFF; 0x7FFFFFFFFFFFFFFF; 0x0000000000000000;
}
# bits-set[6] 262079;
--- stderr

### P10 (6): full resync onto dv-1: down, create-md --force, up while dv-2 and dv-3 are UpToDate; then down
$ [dv-1] drbdadm create-md --force --max-peers=7 p10

You want me to create a v09 style flexible-size internal meta data block.
There appears to be a v09 flexible-size internal meta data block
already in place on /dev/ambar/p10 at byte offset 1073737728

Do you really want to overwrite the existing meta-data?
*** confirmation forced via --force option ***

Do you want to proceed?
*** confirmation forced via --force option ***
initializing bitmap (224 KB) to all zero
md_offset 1073737728
al_offset 1073704960
bm_offset 1073475584

Found ext3 filesystem
     1048316 kB data area apparently used
     1048316 kB left usable by current configuration

Even though it looks like this would place the new meta data into
unused space, you still need to confirm, as this is only a guess.
initializing activity log
Writing meta data...
New drbd meta data block successfully created.
rc=0
$ [dv-1] drbdadm up p10
rc=0
wait: dv-1: own disk UpToDate, two peers UpToDate after 10 s at most
wait: dv-2: own disk UpToDate, two peers UpToDate after 2 s at most
wait: dv-3: own disk UpToDate, two peers UpToDate after 2 s at most
$ [dv-1] drbdsetup status p10 --verbose
p10 node-id:0 role:Secondary suspended:no force-io-failures:no
  volume:0 minor:1010 disk:UpToDate backing_dev:/dev/ambar/p10 quorum:yes open:no
      blocked:no
  lima-dv-2 node-id:1 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
  lima-dv-3 node-id:2 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no

rc=0
$ [dv-2] drbdsetup status p10 --verbose
p10 node-id:1 role:Secondary suspended:no force-io-failures:no
  volume:0 minor:1010 disk:UpToDate backing_dev:/dev/ambar/p10 quorum:yes open:no
      blocked:no
  lima-dv-1 node-id:0 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
  lima-dv-3 node-id:2 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no

rc=0
$ [dv-3] drbdsetup status p10 --verbose
p10 node-id:2 role:Secondary suspended:no force-io-failures:no
  volume:0 minor:1010 disk:UpToDate backing_dev:/dev/ambar/p10 quorum:yes open:no
      blocked:no
  lima-dv-1 node-id:0 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
  lima-dv-2 node-id:1 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no

rc=0
$ [dv-1] drbdadm down p10
rc=0
$ [dv-1] drbdadm dump-md p10
rc=255
--- stdout
--- stderr
Found meta data is "unclean", please apply-al first
Command 'drbdmeta 1010 v09 /dev/ambar/p10 internal dump-md' terminated with exit code 255
--- again after 5 s
$ [dv-1] drbdadm dump-md p10
rc=255
--- stdout
--- stderr
Found meta data is "unclean", please apply-al first
Command 'drbdmeta 1010 v09 /dev/ambar/p10 internal dump-md' terminated with exit code 255
$ [dv-1] drbdadm apply-al p10
rc=0
--- again after apply-al
$ [dv-1] drbdadm dump-md p10
rc=0
--- stdout
# DRBD meta data dump
# 2026-11-13 12:47:06 +0000 [1794574026]
# lima-dv-1> drbdmeta 1010 v09 /dev/ambar/p10 internal dump-md
#

version "v09";

max-peers 7;
# md_size_sect 520
# md_offset 1073737728
# al_offset 1073704960
# bm_offset 1073475584

node-id 0;
current-uuid 0x5E59F70730D0DDAC;
flags 0x00000091;
members 0x0000000000000001;
features 0x0000000000000001;
peer[0] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00010000; # exists
}
peer[1] {
    bitmap-index 1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000010; # seen
}
peer[2] {
    bitmap-index 0;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000010; # seen
}
peer[3] {
    bitmap-index -1;
    bitmap-uuid 0xDFB51D0831B0A7DA;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[4] {
    bitmap-index -1;
    bitmap-uuid 0xDFB51D0831B0A7DA;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[5] {
    bitmap-index -1;
    bitmap-uuid 0xDFB51D0831B0A7DA;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[6] {
    bitmap-index -1;
    bitmap-uuid 0xDFB51D0831B0A7DA;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[7] {
    bitmap-index -1;
    bitmap-uuid 0xDFB51D0831B0A7DA;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[8] {
    bitmap-index -1;
    bitmap-uuid 0xDFB51D0831B0A7DA;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[9] {
    bitmap-index -1;
    bitmap-uuid 0xDFB51D0831B0A7DA;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[10] {
    bitmap-index -1;
    bitmap-uuid 0xDFB51D0831B0A7DA;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[11] {
    bitmap-index -1;
    bitmap-uuid 0xDFB51D0831B0A7DA;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[12] {
    bitmap-index -1;
    bitmap-uuid 0xDFB51D0831B0A7DA;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[13] {
    bitmap-index -1;
    bitmap-uuid 0xDFB51D0831B0A7DA;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[14] {
    bitmap-index -1;
    bitmap-uuid 0xDFB51D0831B0A7DA;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[15] {
    bitmap-index -1;
    bitmap-uuid 0xDFB51D0831B0A7DA;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[16] {
    bitmap-index -1;
    bitmap-uuid 0xDFB51D0831B0A7DA;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[17] {
    bitmap-index -1;
    bitmap-uuid 0xDFB51D0831B0A7DA;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[18] {
    bitmap-index -1;
    bitmap-uuid 0xDFB51D0831B0A7DA;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[19] {
    bitmap-index -1;
    bitmap-uuid 0xDFB51D0831B0A7DA;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[20] {
    bitmap-index -1;
    bitmap-uuid 0xDFB51D0831B0A7DA;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[21] {
    bitmap-index -1;
    bitmap-uuid 0xDFB51D0831B0A7DA;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[22] {
    bitmap-index -1;
    bitmap-uuid 0xDFB51D0831B0A7DA;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[23] {
    bitmap-index -1;
    bitmap-uuid 0xDFB51D0831B0A7DA;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[24] {
    bitmap-index -1;
    bitmap-uuid 0xDFB51D0831B0A7DA;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[25] {
    bitmap-index -1;
    bitmap-uuid 0xDFB51D0831B0A7DA;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[26] {
    bitmap-index -1;
    bitmap-uuid 0xDFB51D0831B0A7DA;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[27] {
    bitmap-index -1;
    bitmap-uuid 0xDFB51D0831B0A7DA;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[28] {
    bitmap-index -1;
    bitmap-uuid 0xDFB51D0831B0A7DA;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[29] {
    bitmap-index -1;
    bitmap-uuid 0xDFB51D0831B0A7DA;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[30] {
    bitmap-index -1;
    bitmap-uuid 0xDFB51D0831B0A7DA;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[31] {
    bitmap-index -1;
    bitmap-uuid 0xDFB51D0831B0A7DA;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
history-uuids {
        0x0240DDD21A4B2CE8; 0x0000000000000000; 0x0000000000000000; 0x0000000000000000;
        0x0000000000000000; 0x0000000000000000; 0x0000000000000000; 0x0000000000000000;
        0x0000000000000000; 0x0000000000000000; 0x0000000000000000; 0x0000000000000000;
        0x0000000000000000; 0x0000000000000000; 0x0000000000000000; 0x0000000000000000;
        0x0000000000000000; 0x0000000000000000; 0x0000000000000000; 0x0000000000000000;
        0x0000000000000000; 0x0000000000000000; 0x0000000000000000; 0x0000000000000000;
        0x0000000000000000; 0x0000000000000000; 0x0000000000000000; 0x0000000000000000;
        0x0000000000000000; 0x0000000000000000; 0x0000000000000000; 0x0000000000000000;
}
# al-extents 1237;
la-size-sect 2096632;
bm-byte-per-bit 4096;
device-uuid 0xF575454F6D1C8D06;
la-peer-max-bio-size 1048576;
al-stripes 1;
al-stripe-size-4k 8;
# bm-bytes 229376;
bitmap[0] {
   # at 0kB
    16 times 0xFFFFFFFFFFFFFFFF;
    4080 times 0x0000000000000000;
}
# bits-set[0] 1024;
bitmap[1] {
   # at 0kB
    4096 times 0x0000000000000000;
}
# bits-set[1] 0;
bitmap[2] {
   # at 0kB
    4092 times 0xFFFFFFFFFFFFFFFF;
    0xFFFFFFFFFFFFFFFF; 0xFFFFFFFFFFFFFFFF; 0x7FFFFFFFFFFFFFFF; 0x0000000000000000;
}
# bits-set[2] 262079;
bitmap[3] {
   # at 0kB
    4092 times 0xFFFFFFFFFFFFFFFF;
    0xFFFFFFFFFFFFFFFF; 0xFFFFFFFFFFFFFFFF; 0x7FFFFFFFFFFFFFFF; 0x0000000000000000;
}
# bits-set[3] 262079;
bitmap[4] {
   # at 0kB
    4092 times 0xFFFFFFFFFFFFFFFF;
    0xFFFFFFFFFFFFFFFF; 0xFFFFFFFFFFFFFFFF; 0x7FFFFFFFFFFFFFFF; 0x0000000000000000;
}
# bits-set[4] 262079;
bitmap[5] {
   # at 0kB
    4092 times 0xFFFFFFFFFFFFFFFF;
    0xFFFFFFFFFFFFFFFF; 0xFFFFFFFFFFFFFFFF; 0x7FFFFFFFFFFFFFFF; 0x0000000000000000;
}
# bits-set[5] 262079;
bitmap[6] {
   # at 0kB
    4092 times 0xFFFFFFFFFFFFFFFF;
    0xFFFFFFFFFFFFFFFF; 0xFFFFFFFFFFFFFFFF; 0x7FFFFFFFFFFFFFFF; 0x0000000000000000;
}
# bits-set[6] 262079;
--- stderr
```

Прогон 2, состояния (1) и (4) (остальные состояния прогона 2 в вывод не включены, их значения — в таблице):

```text
### P10 (1): right after create-md --force --max-peers=7, never up
$ [dv-1] drbdadm dump-md p10
rc=0
--- stdout
# DRBD meta data dump
# 2026-11-13 12:49:00 +0000 [1794574140]
# lima-dv-1> drbdmeta 1010 v09 /dev/ambar/p10 internal dump-md
#

version "v09";

max-peers 7;
# md_size_sect 520
# md_offset 1073737728
# al_offset 1073704960
# bm_offset 1073475584

node-id -1;
current-uuid 0x0000000000000004;
flags 0x00000080;
members 0x0000000000000000;
features 0x0000000000000001;
peer[0] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[1] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[2] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[3] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[4] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[5] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[6] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[7] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[8] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[9] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[10] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[11] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[12] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[13] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[14] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[15] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[16] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[17] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[18] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[19] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[20] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[21] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[22] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[23] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[24] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[25] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[26] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[27] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[28] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[29] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[30] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
peer[31] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000000;
}
history-uuids {
        0x0000000000000000; 0x0000000000000000; 0x0000000000000000; 0x0000000000000000;
        0x0000000000000000; 0x0000000000000000; 0x0000000000000000; 0x0000000000000000;
        0x0000000000000000; 0x0000000000000000; 0x0000000000000000; 0x0000000000000000;
        0x0000000000000000; 0x0000000000000000; 0x0000000000000000; 0x0000000000000000;
        0x0000000000000000; 0x0000000000000000; 0x0000000000000000; 0x0000000000000000;
        0x0000000000000000; 0x0000000000000000; 0x0000000000000000; 0x0000000000000000;
        0x0000000000000000; 0x0000000000000000; 0x0000000000000000; 0x0000000000000000;
        0x0000000000000000; 0x0000000000000000; 0x0000000000000000; 0x0000000000000000;
}
# al-extents 257;
la-size-sect 0;
bm-byte-per-bit 4096;
device-uuid 0x1B25D576AC5F5051;
la-peer-max-bio-size 0;
al-stripes 1;
al-stripe-size-4k 8;
# bm-bytes 0;
bitmap[0] {
}
# bits-set[0] 0;
bitmap[1] {
}
# bits-set[1] 0;
bitmap[2] {
}
# bits-set[2] 0;
bitmap[3] {
}
# bits-set[3] 0;
bitmap[4] {
}
# bits-set[4] 0;
bitmap[5] {
}
# bits-set[5] 0;
bitmap[6] {
}
# bits-set[6] 0;
--- stderr

### P10 (4): dv-1 up again, new-current-uuid --clear-bitmap on dv-1, all UpToDate; then down on dv-1
$ [dv-1] drbdadm up p10
rc=0
wait: dv-1: two connections Connected after 2 s at most
$ [dv-1] drbdadm new-current-uuid --clear-bitmap p10/0
rc=0
wait: dv-1: own disk UpToDate, two peers UpToDate after 2 s at most
wait: dv-2: own disk UpToDate, two peers UpToDate after 2 s at most
wait: dv-3: own disk UpToDate, two peers UpToDate after 2 s at most
$ [dv-1] drbdsetup status p10 --verbose
p10 node-id:0 role:Secondary suspended:no force-io-failures:no
  volume:0 minor:1010 disk:UpToDate backing_dev:/dev/ambar/p10 quorum:yes open:no
      blocked:no
  lima-dv-2 node-id:1 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
  lima-dv-3 node-id:2 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no

rc=0
$ [dv-2] drbdsetup status p10 --verbose
p10 node-id:1 role:Secondary suspended:no force-io-failures:no
  volume:0 minor:1010 disk:UpToDate backing_dev:/dev/ambar/p10 quorum:yes open:no
      blocked:no
  lima-dv-1 node-id:0 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
  lima-dv-3 node-id:2 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no

rc=0
$ [dv-3] drbdsetup status p10 --verbose
p10 node-id:2 role:Secondary suspended:no force-io-failures:no
  volume:0 minor:1010 disk:UpToDate backing_dev:/dev/ambar/p10 quorum:yes open:no
      blocked:no
  lima-dv-1 node-id:0 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
  lima-dv-2 node-id:1 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no

rc=0
$ [dv-1] drbdadm down p10
rc=0
$ [dv-1] drbdadm dump-md p10
rc=255
--- stdout
--- stderr
Found meta data is "unclean", please apply-al first
Command 'drbdmeta 1010 v09 /dev/ambar/p10 internal dump-md' terminated with exit code 255
--- again after 5 s
$ [dv-1] drbdadm dump-md p10
rc=255
--- stdout
--- stderr
Found meta data is "unclean", please apply-al first
Command 'drbdmeta 1010 v09 /dev/ambar/p10 internal dump-md' terminated with exit code 255
$ [dv-1] drbdadm apply-al p10
rc=0
--- again after apply-al
$ [dv-1] drbdadm dump-md p10
rc=0
--- stdout
# DRBD meta data dump
# 2026-11-13 12:49:58 +0000 [1794574198]
# lima-dv-1> drbdmeta 1010 v09 /dev/ambar/p10 internal dump-md
#

version "v09";

max-peers 7;
# md_size_sect 520
# md_offset 1073737728
# al_offset 1073704960
# bm_offset 1073475584

node-id 0;
current-uuid 0x43522DBE2645857A;
flags 0x00000091;
members 0x0000000000000001;
features 0x0000000000000001;
peer[0] {
    bitmap-index -1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00010000; # exists
}
peer[1] {
    bitmap-index 1;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000010; # seen
}
peer[2] {
    bitmap-index 0;
    bitmap-uuid 0x0000000000000000;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000010; # seen
}
peer[3] {
    bitmap-index -1;
    bitmap-uuid 0x7C55463636456D08;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[4] {
    bitmap-index -1;
    bitmap-uuid 0x7C55463636456D08;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[5] {
    bitmap-index -1;
    bitmap-uuid 0x7C55463636456D08;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[6] {
    bitmap-index -1;
    bitmap-uuid 0x7C55463636456D08;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[7] {
    bitmap-index -1;
    bitmap-uuid 0x7C55463636456D08;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[8] {
    bitmap-index -1;
    bitmap-uuid 0x7C55463636456D08;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[9] {
    bitmap-index -1;
    bitmap-uuid 0x7C55463636456D08;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[10] {
    bitmap-index -1;
    bitmap-uuid 0x7C55463636456D08;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[11] {
    bitmap-index -1;
    bitmap-uuid 0x7C55463636456D08;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[12] {
    bitmap-index -1;
    bitmap-uuid 0x7C55463636456D08;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[13] {
    bitmap-index -1;
    bitmap-uuid 0x7C55463636456D08;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[14] {
    bitmap-index -1;
    bitmap-uuid 0x7C55463636456D08;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[15] {
    bitmap-index -1;
    bitmap-uuid 0x7C55463636456D08;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[16] {
    bitmap-index -1;
    bitmap-uuid 0x7C55463636456D08;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[17] {
    bitmap-index -1;
    bitmap-uuid 0x7C55463636456D08;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[18] {
    bitmap-index -1;
    bitmap-uuid 0x7C55463636456D08;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[19] {
    bitmap-index -1;
    bitmap-uuid 0x7C55463636456D08;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[20] {
    bitmap-index -1;
    bitmap-uuid 0x7C55463636456D08;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[21] {
    bitmap-index -1;
    bitmap-uuid 0x7C55463636456D08;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[22] {
    bitmap-index -1;
    bitmap-uuid 0x7C55463636456D08;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[23] {
    bitmap-index -1;
    bitmap-uuid 0x7C55463636456D08;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[24] {
    bitmap-index -1;
    bitmap-uuid 0x7C55463636456D08;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[25] {
    bitmap-index -1;
    bitmap-uuid 0x7C55463636456D08;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[26] {
    bitmap-index -1;
    bitmap-uuid 0x7C55463636456D08;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[27] {
    bitmap-index -1;
    bitmap-uuid 0x7C55463636456D08;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[28] {
    bitmap-index -1;
    bitmap-uuid 0x7C55463636456D08;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[29] {
    bitmap-index -1;
    bitmap-uuid 0x7C55463636456D08;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[30] {
    bitmap-index -1;
    bitmap-uuid 0x7C55463636456D08;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
peer[31] {
    bitmap-index -1;
    bitmap-uuid 0x7C55463636456D08;
    bitmap-dagtag 0x0000000000000000;
    flags 0x00000020; # divergence
}
history-uuids {
        0x0000000000000000; 0x0000000000000000; 0x0000000000000000; 0x0000000000000000;
        0x0000000000000000; 0x0000000000000000; 0x0000000000000000; 0x0000000000000000;
        0x0000000000000000; 0x0000000000000000; 0x0000000000000000; 0x0000000000000000;
        0x0000000000000000; 0x0000000000000000; 0x0000000000000000; 0x0000000000000000;
        0x0000000000000000; 0x0000000000000000; 0x0000000000000000; 0x0000000000000000;
        0x0000000000000000; 0x0000000000000000; 0x0000000000000000; 0x0000000000000000;
        0x0000000000000000; 0x0000000000000000; 0x0000000000000000; 0x0000000000000000;
        0x0000000000000000; 0x0000000000000000; 0x0000000000000000; 0x0000000000000000;
}
# al-extents 1237;
la-size-sect 2096632;
bm-byte-per-bit 4096;
device-uuid 0x1B25D576AC5F5051;
la-peer-max-bio-size 1048576;
al-stripes 1;
al-stripe-size-4k 8;
# bm-bytes 229376;
bitmap[0] {
   # at 0kB
    4096 times 0x0000000000000000;
}
# bits-set[0] 0;
bitmap[1] {
   # at 0kB
    4096 times 0x0000000000000000;
}
# bits-set[1] 0;
bitmap[2] {
   # at 0kB
    4096 times 0x0000000000000000;
}
# bits-set[2] 0;
bitmap[3] {
   # at 0kB
    4096 times 0x0000000000000000;
}
# bits-set[3] 0;
bitmap[4] {
   # at 0kB
    4096 times 0x0000000000000000;
}
# bits-set[4] 0;
bitmap[5] {
   # at 0kB
    4096 times 0x0000000000000000;
}
# bits-set[5] 0;
bitmap[6] {
   # at 0kB
    4096 times 0x0000000000000000;
}
# bits-set[6] 0;
--- stderr
```

### Очистка стенда после опытов P7–P10

```bash
stand/spike/pre-csi.sh clean && stand/check.sh
```

```text
OK: stand is up
```

На трёх узлах `drbdsetup status` — `# No currently configured DRBD found.`, `losetup -l -n | wc -l` = 0, `/dev/ambar` и `/mnt/pa` отсутствуют, в `/var/tmp` только `cloud-init` и каталог `systemd-private-…`. После опытов P10 на `dv-1`: `drbdsetup status` — `# No currently configured DRBD found.`, `/dev/ambar` и `/var/lib/rancher/ambar/volumes` пусты, `stand/check.sh` — `OK: stand is up` (остальные узлы не смотрелись `[проверить: dv-2, dv-3 после P10]`).

## Опыты перед сменой набора копий (P11–P14)

Дата: 2026-11-17. Стенд: четыре ВМ Lima (`dv-1`..`dv-4`), ядро `6.12.116+deb13-arm64`, DRBD 9.3.4, drbd-utils 9.35.0 (версии те же, что в фазе 2а). Опыты выполняет скрипт `stand/spike/pre-2b.sh <p11|p12|p13|p14|clean>`: файлы томов и метаданных изолированы в `/var/lib/rancher/ambar-exp/{volumes,res}` и `/dev/ambar-exp`, выполнение команд идёт в поде `toolbox` пространства имён `ambar-spike` через `stand/lib.sh`. Драйвер `ambar` в пространстве имён `ambar-system`, его DaemonSet и CRD не затрагиваются; томов `ReplicatedVolume` на стенде нет. Перед каждым опытом и после него скрипт выполняет очистку (`cleanup`). Файлы конфигурации ресурсов содержат `options { quorum majority; on-no-quorum io-error; auto-promote yes; }`.

| Опыт | Условия | Команда | Код возврата | Вывод | Прогонов |
|---|---|---|---|---|---|
| P11 (1) | `dv-1`, `dv-2` с копией (1 ГиБ), писатель на `dv-1`, `dv-3` поднят без диска | `drbdadm create-md --force --max-peers=7 p11` на `dv-3` | 0 | `New drbd meta data block successfully created.`; устройство backing_dev не занято ядром в режиме Diskless; статус остаётся `Diskless client:yes` | 2 |
| P11 (2) | то же | `drbdadm down p11` на `dv-3` | 0 | на `dv-1` и `dv-2` связь с `dv-3` переходит в `Connecting role:Unknown`, `peer-disk:DUnknown`, `quorum:yes` сохраняется | 2 |
| P11 (3) | после `down` на `dv-3` | `create-md` и `up` на `dv-3` | 0, 0 | метаданные подтверждены (`--force`), ресурс поднят на `dv-3` | 2 |
| P11 (4) | порядок 1: смена роли на `dv-3` первой, затем на `dv-1`, `dv-2` записан `p11.res` с диском на `dv-3` | `drbdadm adjust p11` на `dv-1` и `dv-2` | 0, 0 | `dv-1` переводит репликацию с `dv-3` в `SyncSource`, `dv-3` — `Inconsistent`; ресинхронизация началась | 2 |
| P11 (5) | опрос `drbdsetup status` на `dv-3` до `UpToDate` | опрос каждые 5 с | 0 | полная синхронизация 1 ГиБ: 88 с (прогон 1), 75 с (прогон 2); `out-of-sync` снизился до 0, `disk:UpToDate` | 2 |
| P11 (6) | остановка писателя на `dv-1` | сравнение подтверждённого номера и номера на томе | — | `acked=34526 on-volume=34527` (прогон 1), `acked=40508 on-volume=40509` (прогон 2); запись не прерывалась, `quorum:yes` непрерывно во время всей смены роли и синхронизации | 2 |
| P11 (7) | порядок 2 (независимые агенты): `adjust` на `dv-1`, `dv-2` пока `dv-3` без диска; затем `down`, `create-md`, `up` на `dv-3` | `adjust` на пирах, затем `down`/`create-md`/`up` на `dv-3` | все 0 | `dv-3` соединяется и переходит в `UpToDate` за 75 с; оба порядка команд функционально эквивалентны | 2 |
| P12 (1) | три копии `p12` (1 ГиБ), писатель на `dv-1`; уход `dv-3` | `drbdadm down p12` на `dv-3` | 0 | на `dv-1` и `dv-2` связь с `dv-3` `Connecting/Outdated`, `quorum:yes` сохраняется непрерывно (2 из 3 копий на месте) | 1 |
| P12 (2) | `p12.res` без `dv-3` на `dv-1` и `dv-2` | `drbdadm adjust p12` на `dv-1`, `dv-2` | 0, 0 | пир `lima-dv-3` удалён из статуса обоих узлов; `quorum:yes` сохраняется, писатель не останавливался | 1 |
| P12 (3) | `dv-1` после ухода `dv-3` (`node-id` 2) | `drbdsetup forget-peer p12 2`, дважды подряд | 0, 0 | stdout и stderr пусты при обоих вызовах; команда идемпотентна | 1 |
| P12 (4) | `dv-2` | `drbdsetup forget-peer p12 5` (узла 5 не было в метаданных) | 0 | stdout и stderr пусты; код возврата 0 | 1 |
| P12 (5) | остановка писателя на `dv-1` | сравнение номеров | — | `acked=23484 on-volume=23485` | 1 |
| P12 (6a) | обратный порядок: уход стороны без диска (`dv-4`); `adjust` на `dv-1` и `dv-2` без `dv-4`, пока `dv-4` поднят | `drbdadm adjust p12` на `dv-1`, `dv-2` | 0, 0 | на `dv-1` и `dv-2` секция `dv-4` удалена; на `dv-4` связи перешли в `Connecting/DUnknown`, `quorum:no` | 1 |
| P12 (6b) | снятие `dv-4` после отключения пиров | `drbdadm down p12` на `dv-4` | 0 | ресурс остановлен; последующий статус даёт код 10 (ресурс не существует); писатель `acked=6979 on-volume=6980` | 1 |
| P13 (1) | `--max-peers=1`, уход `dv-2` без `forget-peer`; добавление `dv-3` (`node-id` 2) | `drbdadm adjust p13` на `dv-1` | 1 | отказ: в dmesg `drbd p13/0 drbd1013 lima-dv-3: Not enough free bitmap slots`; слот остался занят ушедшим `dv-2`; `dv-3` остался `Inconsistent` (`out-of-sync:1048508`) | 1 |
| P13 (2) | `--max-peers=1`, уход `dv-2` с `drbdsetup forget-peer p13 1` на `dv-1`; добавление `dv-3` | `drbdadm adjust p13` на `dv-1` | 0 | в dmesg `Setting and writing one bitmap slot, after forget-peer`; слот освобождён; ресинхронизация прошла успешно до `UpToDate` | 1 |
| P13 (3) | сверка данных после `forget-peer` и синхронизации | `dd ... \| sha256sum` первых 64 МиБ на `dv-1` и `dv-3` | 0, 0 | хэши совпали: `abc698ca27e9875612391be953337219270cbf8090a14a76a64e5ad7caca6770` | 1 |
| P14 (w) | со свидетелем (`dv-4` без диска): копии `dv-1`, `dv-2` (4 ГиБ, 2 ГиБ данных, писатель); добавление `dv-3`; при `dv-3 Inconsistent` — `down` на `dv-2` | `drbdsetup status` на `dv-1` во время падения `dv-2` | 0 | `quorum:yes` непрерывно сохраняется на `dv-1` (соединён с `dv-3 Inconsistent` и `dv-4 Diskless`, 3 из 4 участников); писатель пишет без ошибок; после `up` на `dv-2` все копии возвращаются в `UpToDate` | 2 |
| P14 (wo) | без свидетеля: копии `dv-1`, `dv-2` (4 ГиБ, 2 ГиБ данных, писатель); добавление `dv-3`; при `dv-3 Inconsistent` — `down` на `dv-2` | `drbdsetup status` на `dv-1` во время падения `dv-2` | 0 | `quorum:yes` непрерывно сохраняется на `dv-1` (соединён с `dv-3 Inconsistent`, 2 из 3 участников на связи удовлетворяют большинству); писатель пишет без ошибок; после `up` на `dv-2` все копии возвращаются в `UpToDate` | 2 |

### Выводы по результатам опытов

- **P11 (смена роли «без диска → с копией»):**
  1. Вызов `drbdadm create-md` на узле, где ресурс поднят без диска (`Diskless`), завершается успешно (код 0), поскольку в бездисковом режиме блочное устройство не занято ядром. Однако применение диска требует перезапуска ресурса на узле (`down` и последующий `up`).
  2. Полная синхронизация тома на добавляемом узле при активном писателе на Primary проходит без сбоев. Для 1 ГиБ время синхронизации составило 88 с в первом прогоне и 75 с во втором.
  3. Запись на Primary не прерывалась ни на одном этапе (номера `acked` и `on-volume` расходятся ровно на единицу последнего незавершённого блока).
  4. Кворум на первичном узле (`quorum:yes`) сохранялся непрерывно в обоих порядках применения.
  5. Команда `drbdadm adjust` на существующих узлах необходима для уведомления ядра о появлении диска у пира и перевода связи в режим синхронизации.
  6. Оба порядка команд («сначала узел меняет роль на дисковую, затем остальные выполняют `adjust`» и «сначала остальные выполняют `adjust`, пока узел работает без диска, затем узел перезапускается с диском») работают стабильно и приводят узел к состоянию `UpToDate`.

- **P12 (уход узла):**
  1. При отключении одной из трёх сторон с копией первичный узел сохраняет кворум (`quorum:yes`) как до выполнения `adjust` на оставшихся узлах, так и после него.
  2. Команда `drbdsetup forget-peer <resource> <node-id>` завершается с кодом 0 и не выводит текста в stdout/stderr. Она идемпотентна: повторный вызов для того же узла и вызов для несуществующего `node-id` также возвращают код 0 без сообщений.
  3. При уходе стороны без диска выполнение `adjust` на оставшихся узлах отключает пир; сам узел без диска переходит в статус `Connecting/DUnknown` с `quorum:no` и корректно останавливается последующим `drbdadm down` (код 0).

- **P13 (слоты битовой карты):**
  1. При ограничении числа слотов (`--max-peers=1`) вывод узла из конфигурации без вызова `forget-peer` оставляет слот битовой карты занятым. Попытка подключить новый узел с другим `node-id` завершается ошибкой `adjust` (код 1) с записью в системный журнал `Not enough free bitmap slots`, ресинхронизация не происходит.
  2. Вызов `drbdsetup forget-peer` освобождает слот в метаданных (`Setting and writing one bitmap slot, after forget-peer`), после чего новый узел успешно подключается и синхронизируется.
  3. Контрольные суммы SHA-256 (64 МиБ данных) на первичном узле и на новом синхронизированном узле полностью идентичны (`abc698ca27e9875612391be953337219270cbf8090a14a76a64e5ad7caca6770`).

- **P14 (кворум при синхронизирующейся копии):**
  1. Копия в состоянии `Inconsistent` учитывается модулем DRBD как участник кворума (подключённый узел).
  2. Первичный узел сохраняет кворум и не прерывает запись при падении второй копии как в конфигурации со свидетелем (3 из 4 участников на связи), так и в конфигурации без свидетеля (2 из 3 участников на связи: Primary + Inconsistent). В обоих случаях запись продолжалась без ошибок ввода-вывода.

### Отклонения и правки скрипта

| Место | Что произошло |
|---|---|
| P14, вариант без свидетеля (первый запуск) | При синхронизации тома 4 ГиБ с отключением и возвратом соседа время ожидания превысило начальный лимит функции `wait_status` (600 с), скрипт завершился по таймауту. Лимит ожидания в `stand/spike/pre-2b.sh` был увеличен до 1200 с. Повторный запуск варианта без свидетеля (`P14_VARIANTS=0 stand/spike/pre-2b.sh p14`) прошёл успешно в обоих прогонах (код возврата 0). |

### P11. Сторона без диска становится стороной с копией

Ресурс `p11`: `dv-1`, `dv-2` с копией (1 ГиБ), `dv-3` без диска, port 7011, minor 1011.

```bash
stand/spike/pre-2b.sh p11
```

```text
### P11 run 1: dv-3 changes first, then adjust on dv-1 and dv-2
$ [dv-1] drbdadm create-md --force --max-peers=7 p11
initializing bitmap (224 KB) to all zero
initializing activity log
Writing meta data...
New drbd meta data block successfully created.
rc=0
$ [dv-2] drbdadm create-md --force --max-peers=7 p11
initializing bitmap (224 KB) to all zero
initializing activity log
Writing meta data...
New drbd meta data block successfully created.
rc=0
$ [dv-1] drbdadm up p11
rc=0
$ [dv-2] drbdadm up p11
rc=0
$ [dv-3] drbdadm up p11
rc=0
wait: dv-1: 2 connections Connected after 2 s at most
wait: dv-2: 2 connections Connected after 2 s at most
wait: dv-3: 2 connections Connected after 2 s at most
$ [dv-1] drbdadm new-current-uuid --clear-bitmap p11/0
rc=0
$ [dv-1] drbdsetup status p11 --verbose
p11 node-id:0 role:Secondary suspended:no force-io-failures:no
  volume:0 minor:1011 disk:UpToDate backing_dev:/dev/ambar-exp/p11 quorum:yes
      open:no blocked:no
  lima-dv-2 node-id:1 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
  lima-dv-3 node-id:2 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:Diskless peer-client:yes
        resync-suspended:no

rc=0
$ [dv-1] mkfs.ext4 -q -F -E nodiscard /dev/drbd1011
rc=0
19:48:50.269
1244
started

### P11 run 1, step 1: create-md on dv-3 while the resource runs without a disk there
@ 19:48:53.121 create-md on the running diskless resource
$ [dv-3] drbdadm create-md --force --max-peers=7 p11
initializing bitmap (224 KB) to all zero
initializing activity log
Writing meta data...
New drbd meta data block successfully created.
rc=0
$ [dv-3] drbdsetup status p11 --verbose
p11 node-id:2 role:Secondary suspended:no force-io-failures:no
  volume:0 minor:1011 disk:Diskless client:yes backing_dev:none quorum:yes open:no
      blocked:no
  lima-dv-1 node-id:0 connection:Connected role:Primary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
  lima-dv-2 node-id:1 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no

rc=0

### P11 run 1, step 2: down on dv-3, status of dv-1 and dv-2
@ 19:48:55.726 down on dv-3
$ [dv-3] drbdadm down p11
rc=0
$ [dv-1] drbdsetup status p11 --verbose
p11 node-id:0 role:Primary suspended:no force-io-failures:no
  volume:0 minor:1011 disk:UpToDate backing_dev:/dev/ambar-exp/p11 quorum:yes
      open:yes blocked:no
  lima-dv-2 node-id:1 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
  lima-dv-3 node-id:2 connection:Connecting role:Unknown tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Off peer-disk:DUnknown resync-suspended:no

rc=0
$ [dv-2] drbdsetup status p11 --verbose
p11 node-id:1 role:Secondary suspended:no force-io-failures:no
  volume:0 minor:1011 disk:UpToDate backing_dev:/dev/ambar-exp/p11 quorum:yes
      open:no blocked:no
  lima-dv-1 node-id:0 connection:Connected role:Primary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
  lima-dv-3 node-id:2 connection:Connecting role:Unknown tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Off peer-disk:DUnknown resync-suspended:no

rc=0

### P11 run 1, step 3: create-md and up on dv-3
@ 19:49:02.666 create-md on dv-3 after down
$ [dv-3] drbdadm create-md --force --max-peers=7 p11

You want me to create a v09 style flexible-size internal meta data block.
There appears to be a v09 flexible-size internal meta data block
already in place on /dev/ambar-exp/p11 at byte offset 1073737728

Do you really want to overwrite the existing meta-data?
*** confirmation forced via --force option ***
initializing bitmap (224 KB) to all zero
initializing activity log
Writing meta data...
New drbd meta data block successfully created.
rc=0
$ [dv-3] drbdadm up p11
rc=0
@ 19:49:05.026 up on dv-3 (UP_T0)

### P11 run 1, step 4: p11.res with a disk on dv-3 on dv-1 and dv-2, adjust
@ 19:49:06.496 adjust on dv-1
$ [dv-1] drbdadm adjust p11
rc=0
$ [dv-2] drbdadm adjust p11
rc=0
$ [dv-1] drbdsetup status p11 --verbose --statistics
p11 node-id:0 role:Primary suspended:no force-io-failures:no
    write-ordering:flush
  volume:0 minor:1011 disk:UpToDate backing_dev:/dev/ambar-exp/p11 quorum:yes
      open:yes
      size:1048316 read:104429 written:155364 al-writes:9 bm-writes:128 upper-pending:2
      lower-pending:3 al-suspended:no blocked:no
  lima-dv-2 node-id:1 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:24 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
        received:0 sent:122476 out-of-sync:0 pending:2 unacked:0
  lima-dv-3 node-id:2 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:SyncSource peer-disk:Inconsistent done:9.87
        resync-suspended:no
        received:0 sent:103424 out-of-sync:944892 pending:0 unacked:1 dbdt1:20.75 eta:44

rc=0
$ [dv-2] drbdsetup status p11 --verbose --statistics
p11 node-id:1 role:Secondary suspended:no force-io-failures:no
    write-ordering:flush
  volume:0 minor:1011 disk:UpToDate backing_dev:/dev/ambar-exp/p11 quorum:yes
      open:no
      size:1048316 read:0 written:159448 al-writes:10 bm-writes:125 upper-pending:0
      lower-pending:1 al-suspended:no blocked:no
  lima-dv-1 node-id:0 connection:Connected role:Primary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
        received:126552 sent:0 out-of-sync:0 pending:0 unacked:0
  lima-dv-3 node-id:2 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:PausedSyncS peer-disk:Inconsistent done:8.21
        resync-suspended:peer
        received:0 sent:0 out-of-sync:962300 pending:0 unacked:0 dbdt1:17.13 eta:55

rc=0
$ [dv-3] drbdsetup status p11 --verbose --statistics
p11 node-id:2 role:Secondary suspended:no force-io-failures:no
    write-ordering:flush
  volume:0 minor:1011 disk:Inconsistent backing_dev:/dev/ambar-exp/p11 quorum:yes
      open:no
      size:1048316 read:0 written:209920 al-writes:0 bm-writes:72 upper-pending:0
      lower-pending:0 al-suspended:no blocked:no
  lima-dv-1 node-id:0 connection:Connected role:Primary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:SyncTarget peer-disk:UpToDate done:20.02 resync-suspended:no
        received:209920 sent:0 out-of-sync:838396 pending:1 unacked:0 dbdt1:29.83 eta:27
  lima-dv-2 node-id:1 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:PausedSyncT peer-disk:UpToDate done:12.89
        resync-suspended:dependency
        received:0 sent:0 out-of-sync:913148 pending:0 unacked:0 dbdt1:21.91 eta:41

rc=0

### P11 run 1, step 5: wait for UpToDate on dv-3
t+12 s [dv-3]
  volume:0 minor:1011 disk:Inconsistent backing_dev:/dev/ambar-exp/p11 quorum:yes
  volume:0 replication:SyncTarget peer-disk:UpToDate done:26.26 resync-suspended:no
  received:275456 sent:0 out-of-sync:773508 pending:8 unacked:0 dbdt1:34.08 eta:22
  volume:0 replication:PausedSyncT peer-disk:UpToDate done:18.75
  received:0 sent:0 out-of-sync:851712 pending:0 unacked:0 dbdt1:26.10 eta:32
t+19 s [dv-3]
  volume:0 minor:1011 disk:Inconsistent backing_dev:/dev/ambar-exp/p11 quorum:yes
  volume:0 replication:SyncTarget peer-disk:UpToDate done:44.15 resync-suspended:no
  received:470024 sent:0 out-of-sync:594688 pending:9 unacked:0 dbdt1:26.67 eta:22
  volume:0 replication:PausedSyncT peer-disk:UpToDate done:36.69
  received:0 sent:0 out-of-sync:671488 pending:0 unacked:0 dbdt1:26.96 eta:24
t+25 s [dv-3]
  volume:0 minor:1011 disk:Inconsistent backing_dev:/dev/ambar-exp/p11 quorum:yes
  volume:0 replication:SyncTarget peer-disk:UpToDate done:44.39 resync-suspended:no
  received:472608 sent:0 out-of-sync:592104 pending:5 unacked:0 dbdt1:0.42 eta:1373
  volume:0 replication:PausedSyncT peer-disk:UpToDate done:37.07
  received:0 sent:0 out-of-sync:667392 pending:0 unacked:0 dbdt1:18.89 eta:35
t+31 s [dv-3]
  volume:0 minor:1011 disk:Inconsistent backing_dev:/dev/ambar-exp/p11 quorum:yes
  volume:0 replication:SyncTarget peer-disk:UpToDate done:48.87 resync-suspended:no
  received:520320 sent:0 out-of-sync:544392 pending:5 unacked:0 dbdt1:1.82 eta:293
  volume:0 replication:PausedSyncT peer-disk:UpToDate done:41.71
  received:0 sent:0 out-of-sync:618240 pending:0 unacked:0 dbdt1:16.05 eta:38
t+37 s [dv-3]
  volume:0 minor:1011 disk:Inconsistent backing_dev:/dev/ambar-exp/p11 quorum:yes
  volume:0 replication:SyncTarget peer-disk:UpToDate done:55.40 resync-suspended:no
  received:589824 sent:0 out-of-sync:474904 pending:6 unacked:0 dbdt1:6.06 eta:77
  volume:0 replication:PausedSyncT peer-disk:UpToDate done:47.89
  received:0 sent:0 out-of-sync:552704 pending:0 unacked:0 dbdt1:14.99 eta:36
t+44 s [dv-3]
  volume:0 minor:1011 disk:Inconsistent backing_dev:/dev/ambar-exp/p11 quorum:yes
  volume:0 replication:SyncTarget peer-disk:UpToDate done:59.53 resync-suspended:no
  received:633856 sent:0 out-of-sync:430880 pending:1 unacked:0 dbdt1:10.48 eta:40
  volume:0 replication:PausedSyncT peer-disk:UpToDate done:51.75
  received:0 sent:0 out-of-sync:511744 pending:0 unacked:0 dbdt1:13.44 eta:37
t+50 s [dv-3]
  volume:0 minor:1011 disk:Inconsistent backing_dev:/dev/ambar-exp/p11 quorum:yes
  volume:0 replication:SyncTarget peer-disk:UpToDate done:73.67 resync-suspended:no
  received:784448 sent:0 out-of-sync:280300 pending:9 unacked:0 dbdt1:19.48 eta:14
  volume:0 replication:PausedSyncT peer-disk:UpToDate done:66.04
  received:0 sent:0 out-of-sync:360192 pending:0 unacked:0 dbdt1:14.86 eta:24
t+56 s [dv-3]
  volume:0 minor:1011 disk:Inconsistent backing_dev:/dev/ambar-exp/p11 quorum:yes
  volume:0 replication:SyncTarget peer-disk:UpToDate done:74.45 resync-suspended:no
  received:792752 sent:0 out-of-sync:272008 pending:5 unacked:0 dbdt1:0.40 eta:662
  volume:0 replication:PausedSyncT peer-disk:UpToDate done:66.81
  received:0 sent:0 out-of-sync:352000 pending:0 unacked:0 dbdt1:13.21 eta:26
t+62 s [dv-3]
  volume:0 minor:1011 disk:Inconsistent backing_dev:/dev/ambar-exp/p11 quorum:yes
  volume:0 replication:SyncTarget peer-disk:UpToDate done:84.68 resync-suspended:no
  received:901696 sent:0 out-of-sync:163080 pending:6 unacked:0 dbdt1:25.55 eta:6
  volume:0 replication:PausedSyncT peer-disk:UpToDate done:77.24
  received:0 sent:0 out-of-sync:241408 pending:0 unacked:0 dbdt1:13.70 eta:17
t+69 s [dv-3]
  volume:0 minor:1011 disk:Inconsistent backing_dev:/dev/ambar-exp/p11 quorum:yes
  volume:0 replication:SyncTarget peer-disk:UpToDate done:89.53 resync-suspended:no
  received:953344 sent:0 out-of-sync:111448 pending:8 unacked:0 dbdt1:11.52 eta:9
  volume:0 replication:PausedSyncT peer-disk:UpToDate done:81.87
  received:0 sent:0 out-of-sync:192256 pending:0 unacked:0 dbdt1:13.14 eta:14
t+75 s [dv-3]
  volume:0 minor:1011 disk:Inconsistent backing_dev:/dev/ambar-exp/p11 quorum:yes
  volume:0 replication:SyncTarget peer-disk:UpToDate done:92.38 resync-suspended:no
  received:983688 sent:0 out-of-sync:81120 pending:6 unacked:0 dbdt1:1.67 eta:47
  volume:0 replication:PausedSyncT peer-disk:UpToDate done:84.96
  received:0 sent:0 out-of-sync:159488 pending:0 unacked:0 dbdt1:12.39 eta:13
t+82 s [dv-3]
  volume:0 minor:1011 disk:Inconsistent backing_dev:/dev/ambar-exp/p11 quorum:yes
  volume:0 replication:SyncTarget peer-disk:UpToDate done:98.67 resync-suspended:no
  received:1050632 sent:0 out-of-sync:14180 pending:2 unacked:0 dbdt1:12.78 eta:1
  volume:0 replication:PausedSyncT peer-disk:UpToDate done:91.50
  received:0 sent:0 out-of-sync:90112 pending:0 unacked:0 dbdt1:12.21 eta:7
t+88 s [dv-3]
  volume:0 minor:1011 disk:UpToDate backing_dev:/dev/ambar-exp/p11 quorum:yes
  volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
  received:1071460 sent:0 out-of-sync:0 pending:0 unacked:1
  volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
  received:0 sent:0 out-of-sync:72660 pending:0 unacked:0
UpToDate on dv-3: t+88 s after the mark (poll step 5 s plus the call latency)
$ [dv-1] drbdsetup status p11 --verbose --statistics
p11 node-id:0 role:Primary suspended:no force-io-failures:no
    write-ordering:flush
  volume:0 minor:1011 disk:UpToDate backing_dev:/dev/ambar-exp/p11 quorum:yes
      open:yes
      size:1048316 read:1070329 written:579168 al-writes:10 bm-writes:223
      upper-pending:1 lower-pending:2 al-suspended:no blocked:no
  lima-dv-2 node-id:1 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:8 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
        received:0 sent:546276 out-of-sync:0 pending:1 unacked:0
  lima-dv-3 node-id:2 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:8 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
        received:0 sent:1086380 out-of-sync:4 pending:1 unacked:0

rc=0
$ [dv-2] drbdsetup status p11 --verbose --statistics
p11 node-id:1 role:Secondary suspended:no force-io-failures:no
    write-ordering:flush
  volume:0 minor:1011 disk:UpToDate backing_dev:/dev/ambar-exp/p11 quorum:yes
      open:no
      size:1048316 read:0 written:582764 al-writes:11 bm-writes:206 upper-pending:0
      lower-pending:1 al-suspended:no blocked:no
  lima-dv-1 node-id:0 connection:Connected role:Primary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
        received:549872 sent:0 out-of-sync:0 pending:0 unacked:1
  lima-dv-3 node-id:2 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
        received:0 sent:0 out-of-sync:71132 pending:0 unacked:0

rc=0
$ [dv-3] drbdsetup status p11 --verbose --statistics
p11 node-id:2 role:Secondary suspended:no force-io-failures:no
    write-ordering:flush
  volume:0 minor:1011 disk:UpToDate backing_dev:/dev/ambar-exp/p11 quorum:yes
      open:no
      size:1048316 read:0 written:1093428 al-writes:5 bm-writes:190 upper-pending:0
      lower-pending:1 al-suspended:no blocked:no
  lima-dv-1 node-id:0 connection:Connected role:Primary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
        received:1093432 sent:0 out-of-sync:0 pending:0 unacked:1
  lima-dv-2 node-id:1 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
        received:0 sent:0 out-of-sync:69632 pending:0 unacked:0

rc=0

### P11 run 1, step 6: stop the writer, quorum log, kernel logs
@ 19:50:40.685 stop the writer
acked=34526 on-volume=34527
--- probe.sh log (stdout and stderr)
--- watch.sh log
samples=225 probe=alive:R=15 probe=alive:S=210 longest-gap-without-new-acked=0.6s (from 19:50:22.092)
--- quorum log of dv-1 (runs of equal values: first - last  value  samples)
19:48:51.782 - 19:50:41.541  "quorum": true  x110
--- dmesg -T | grep 'drbd p11' | tail -n 80 [dv-1]
[Tue Nov 17 19:48:37 2026] drbd p11/0 drbd1011: persisting effective size = 1024 MB (1048316 KB)
[Tue Nov 17 19:48:42 2026] drbd p11: Preparing cluster-wide state change 1425730800: 0->all role( Primary )
[Tue Nov 17 19:48:42 2026] drbd p11: State change 1425730800: primary_nodes=1, weak_nodes=FFFFFFFFFFFFFFF8
[Tue Nov 17 19:48:42 2026] drbd p11: Committing cluster-wide state change 1425730800 (0ms)
[Tue Nov 17 19:48:42 2026] drbd p11: role( Secondary -> Primary ) [mkfs.ext4:170684 auto-promote]
[Tue Nov 17 19:48:42 2026] drbd p11/0 drbd1011: gen-obligation: NONE -> ARMED(promoted)
[Tue Nov 17 19:48:42 2026] drbd p11/0 drbd1011: gen-obligation: ARMED(promoted) -> MINTING(promoted)
[Tue Nov 17 19:48:42 2026] drbd p11/0 drbd1011: gen-obligation: MINTING(promoted) -> DISCHARGED(promoted)
[Tue Nov 17 19:48:42 2026] drbd p11: Preparing cluster-wide state change 1077584153: 0->all role( Secondary )
[Tue Nov 17 19:48:42 2026] drbd p11: State change 1077584153: primary_nodes=0, weak_nodes=0
[Tue Nov 17 19:48:42 2026] drbd p11: Committing cluster-wide state change 1077584153 (0ms)
[Tue Nov 17 19:48:42 2026] drbd p11: role( Primary -> Secondary ) [mkfs.ext4:170684 auto-demote]
[Tue Nov 17 19:48:43 2026] drbd p11: Preparing cluster-wide state change 2208671943: 0->all role( Primary )
[Tue Nov 17 19:48:43 2026] drbd p11: State change 2208671943: primary_nodes=1, weak_nodes=FFFFFFFFFFFFFFF8
[Tue Nov 17 19:48:43 2026] drbd p11: Committing cluster-wide state change 2208671943 (0ms)
[Tue Nov 17 19:48:43 2026] drbd p11: role( Secondary -> Primary ) [mount:170709 auto-promote]
[Tue Nov 17 19:48:43 2026] drbd p11/0 drbd1011: gen-obligation: DISCHARGED(promoted) -> ARMED(promoted)
[Tue Nov 17 19:48:43 2026] drbd p11/0 drbd1011: gen-obligation: ARMED(promoted) -> MINTING(promoted)
[Tue Nov 17 19:48:43 2026] drbd p11/0 drbd1011: gen-obligation: MINTING(promoted) -> DISCHARGED(promoted)
[Tue Nov 17 19:48:54 2026] drbd p11: Preparing remote state change 2277214307: 2->0 conn( Disconnecting ) disk( Outdated )
[Tue Nov 17 19:48:54 2026] drbd p11 lima-dv-3: Committing remote state change 2277214307 (primary_nodes=1)
[Tue Nov 17 19:48:54 2026] drbd p11 lima-dv-3: conn( Connected -> TearDown ) peer( Secondary -> Unknown ) [remote]
[Tue Nov 17 19:48:54 2026] drbd p11/0 drbd1011 lima-dv-3: pdsk( Diskless -> DUnknown ) repl( Established -> Off ) [remote]
[Tue Nov 17 19:48:54 2026] drbd p11 lima-dv-3: Terminating sender thread
[Tue Nov 17 19:48:54 2026] drbd p11 lima-dv-3: Starting sender thread (peer-node-id 2)
[Tue Nov 17 19:48:54 2026] drbd p11 lima-dv-3: Connection closed
[Tue Nov 17 19:48:54 2026] drbd p11 lima-dv-3: conn( TearDown -> Unconnected ) [disconnected]
[Tue Nov 17 19:48:54 2026] drbd p11 lima-dv-3: Restarting receiver thread
[Tue Nov 17 19:48:54 2026] drbd p11 lima-dv-3: conn( Unconnected -> Connecting ) [connecting]
[Tue Nov 17 19:48:54 2026] drbd p11: Preparing remote state change 4255602590: 2->1 conn( Disconnecting )
[Tue Nov 17 19:48:54 2026] drbd p11 lima-dv-2: Committing remote state change 4255602590 (primary_nodes=1)
[Tue Nov 17 19:49:03 2026] drbd p11 lima-dv-3: Handshake to peer 2 successful: Agreed network protocol version 125
[Tue Nov 17 19:49:03 2026] drbd p11 lima-dv-3: Feature flags enabled on protocol level: 0x7ff TRIM THIN_RESYNC WRITE_SAME WRITE_ZEROES RESYNC_DAGTAG WRITE_POSTPONE
[Tue Nov 17 19:49:03 2026] drbd p11: Preparing cluster-wide state change 590312596: 0->2 role( Primary ) conn( Connected )
[Tue Nov 17 19:49:03 2026] drbd p11/0 drbd1011 lima-dv-3: The peer is configured to be diskless but presents Inconsistent
[Tue Nov 17 19:49:03 2026] drbd p11 lima-dv-3: conn( Connecting -> Disconnecting )
[Tue Nov 17 19:49:03 2026] drbd p11 lima-dv-3: error receiving P_STATE, e: -5 l: 0!
[Tue Nov 17 19:49:03 2026] drbd p11: Aborting cluster-wide state change 590312596 (12ms) rv = -21
[Tue Nov 17 19:49:03 2026] drbd p11 lima-dv-3: Failure to connect: Interrupted state change (-21); retrying
[Tue Nov 17 19:49:03 2026] drbd p11 lima-dv-3: Terminating sender thread
[Tue Nov 17 19:49:03 2026] drbd p11 lima-dv-3: Starting sender thread (peer-node-id 2)
[Tue Nov 17 19:49:03 2026] drbd p11: Preparing remote state change 3042428756: 1->2 role( Secondary ) conn( Connected )
[Tue Nov 17 19:49:03 2026] drbd p11 lima-dv-2: Aborting remote state change 3042428756
[Tue Nov 17 19:49:03 2026] drbd p11 lima-dv-3: Connection closed
[Tue Nov 17 19:49:03 2026] drbd p11 lima-dv-3: conn( Disconnecting -> StandAlone ) [disconnected]
[Tue Nov 17 19:49:03 2026] drbd p11 lima-dv-3: Terminating receiver thread
[Tue Nov 17 19:49:03 2026] drbd p11: Preparing remote state change 1767100898: 1->2 role( Secondary ) conn( Connected )
[Tue Nov 17 19:49:03 2026] drbd p11 lima-dv-2: Aborting remote state change 1767100898
[Tue Nov 17 19:49:05 2026] drbd p11/0 drbd1011 lima-dv-3: Former intentional diskless peer got bitmap slot 1
[Tue Nov 17 19:49:05 2026] drbd p11 lima-dv-3: conn( StandAlone -> Unconnected ) [connect]
[Tue Nov 17 19:49:05 2026] drbd p11 lima-dv-3: Starting receiver thread (peer-node-id 2)
[Tue Nov 17 19:49:05 2026] drbd p11 lima-dv-3: conn( Unconnected -> Connecting ) [connecting]
[Tue Nov 17 19:49:05 2026] drbd p11 lima-dv-3: Handshake to peer 2 successful: Agreed network protocol version 125
[Tue Nov 17 19:49:05 2026] drbd p11 lima-dv-3: Feature flags enabled on protocol level: 0x7ff TRIM THIN_RESYNC WRITE_SAME WRITE_ZEROES RESYNC_DAGTAG WRITE_POSTPONE
[Tue Nov 17 19:49:05 2026] drbd p11: Preparing cluster-wide state change 1875635909: 0->2 role( Primary ) conn( Connected )
[Tue Nov 17 19:49:05 2026] drbd p11/0 drbd1011 lima-dv-3: drbd_sync_handshake:
[Tue Nov 17 19:49:05 2026] drbd p11/0 drbd1011 lima-dv-3: self 3F9C373A6A3CE662:0000000000000000:0000000000000000:0000000000000000 bits:8445 flags:120
[Tue Nov 17 19:49:05 2026] drbd p11/0 drbd1011 lima-dv-3: peer 0000000000000004:0000000000000000:0000000000000000:0000000000000000 bits:262079 flags:2024
[Tue Nov 17 19:49:05 2026] drbd p11/0 drbd1011 lima-dv-3: uuid_compare()=source-set-bitmap by rule=just-created-peer
[Tue Nov 17 19:49:05 2026] drbd p11/0 drbd1011 lima-dv-3: Setting and writing one bitmap slot, after drbd_sync_handshake
[Tue Nov 17 19:49:05 2026] drbd p11: State change 1875635909: primary_nodes=1, weak_nodes=FFFFFFFFFFFFFFF8
[Tue Nov 17 19:49:05 2026] drbd p11: Committing cluster-wide state change 1875635909 (16ms)
[Tue Nov 17 19:49:05 2026] drbd p11 lima-dv-3: conn( Connecting -> Connected ) peer( Unknown -> Secondary ) [connected]
[Tue Nov 17 19:49:05 2026] drbd p11/0 drbd1011 lima-dv-3: pdsk( DUnknown -> Inconsistent ) repl( Off -> WFBitMapS ) replication( yes -> no ) [connected]
[Tue Nov 17 19:49:05 2026] drbd p11/0 drbd1011 lima-dv-3: send bitmap stats [Bytes(packets)]: plain 0(0), RLE 21(1), total 21; compression: 100.0%
[Tue Nov 17 19:49:05 2026] drbd p11/0 drbd1011 lima-dv-3: receive bitmap stats [Bytes(packets)]: plain 0(0), RLE 21(1), total 21; compression: 100.0%
[Tue Nov 17 19:49:05 2026] drbd p11/0 drbd1011 lima-dv-3: repl( WFBitMapS -> SyncSource ) [receive-bitmap]
[Tue Nov 17 19:49:05 2026] drbd p11/0 drbd1011 lima-dv-3: Began resync as SyncSource (will sync 1048316 KB [262079 bits set]).
[Tue Nov 17 19:49:07 2026] drbd p11: Preparing remote state change 1944363446: 1->2 role( Secondary ) conn( Connected )
[Tue Nov 17 19:49:07 2026] drbd p11 lima-dv-2: Committing remote state change 1944363446 (primary_nodes=1)
[Tue Nov 17 19:49:18 2026] drbd p11/0 drbd1011: md_sync_timer expired! Worker calls drbd_md_sync().
[Tue Nov 17 19:50:28 2026] drbd p11/0 drbd1011 lima-dv-3: replication( no -> yes ) [enable-replication]
[Tue Nov 17 19:50:30 2026] drbd p11/0 drbd1011 lima-dv-3: Resync done (total 84 sec; paused 0 sec; 12728 K/sec)
[Tue Nov 17 19:50:30 2026] drbd p11/0 drbd1011 lima-dv-3: pdsk( Inconsistent -> UpToDate ) repl( SyncSource -> Established ) [resync-finished]
[Tue Nov 17 19:50:39 2026] drbd p11: Preparing cluster-wide state change 1203004661: 0->all role( Secondary )
[Tue Nov 17 19:50:39 2026] drbd p11: State change 1203004661: primary_nodes=0, weak_nodes=0
[Tue Nov 17 19:50:39 2026] drbd p11: Committing cluster-wide state change 1203004661 (0ms)
[Tue Nov 17 19:50:39 2026] drbd p11: role( Primary -> Secondary ) [umount:210966 auto-demote]
[Tue Nov 17 19:50:39 2026] drbd p11/0 drbd1011 lima-dv-3: pdsk( UpToDate -> Outdated ) [peer-state]
[Tue Nov 17 19:50:39 2026] drbd p11/0 drbd1011 lima-dv-3: pdsk( Outdated -> Inconsistent ) resync-susp( no -> peer ) replication( yes -> no ) [peer-state]
--- dmesg -T | grep 'drbd p11' | tail -n 80 [dv-2]
[Tue Nov 17 19:48:42 2026] drbd p11: Preparing remote state change 1425730800: 0->all role( Primary )
[Tue Nov 17 19:48:42 2026] drbd p11 lima-dv-1: Committing remote state change 1425730800 (primary_nodes=1)
[Tue Nov 17 19:48:42 2026] drbd p11 lima-dv-1: peer( Secondary -> Primary ) [remote]
[Tue Nov 17 19:48:42 2026] drbd p11: Preparing remote state change 1077584153: 0->all role( Secondary )
[Tue Nov 17 19:48:42 2026] drbd p11 lima-dv-1: Committing remote state change 1077584153 (primary_nodes=0)
[Tue Nov 17 19:48:42 2026] drbd p11 lima-dv-1: peer( Primary -> Secondary ) [remote]
[Tue Nov 17 19:48:43 2026] drbd p11: Preparing remote state change 2208671943: 0->all role( Primary )
[Tue Nov 17 19:48:43 2026] drbd p11 lima-dv-1: Committing remote state change 2208671943 (primary_nodes=1)
[Tue Nov 17 19:48:43 2026] drbd p11 lima-dv-1: peer( Secondary -> Primary ) [remote]
[Tue Nov 17 19:48:54 2026] drbd p11: Preparing remote state change 2277214307: 2->0 conn( Disconnecting ) disk( Outdated )
[Tue Nov 17 19:48:54 2026] drbd p11 lima-dv-3: Committing remote state change 2277214307 (primary_nodes=1)
[Tue Nov 17 19:48:54 2026] drbd p11: Preparing remote state change 4255602590: 2->1 conn( Disconnecting )
[Tue Nov 17 19:48:54 2026] drbd p11 lima-dv-3: Committing remote state change 4255602590 (primary_nodes=1)
[Tue Nov 17 19:48:54 2026] drbd p11 lima-dv-3: conn( Connected -> TearDown ) peer( Secondary -> Unknown ) [remote]
[Tue Nov 17 19:48:54 2026] drbd p11/0 drbd1011 lima-dv-3: pdsk( Diskless -> DUnknown ) repl( Established -> Off ) [remote]
[Tue Nov 17 19:48:54 2026] drbd p11 lima-dv-3: Terminating sender thread
[Tue Nov 17 19:48:54 2026] drbd p11 lima-dv-3: Starting sender thread (peer-node-id 2)
[Tue Nov 17 19:48:54 2026] drbd p11 lima-dv-3: Connection closed
[Tue Nov 17 19:48:54 2026] drbd p11 lima-dv-3: conn( TearDown -> Unconnected ) [disconnected]
[Tue Nov 17 19:48:54 2026] drbd p11 lima-dv-3: Restarting receiver thread
[Tue Nov 17 19:48:54 2026] drbd p11 lima-dv-3: conn( Unconnected -> Connecting ) [connecting]
[Tue Nov 17 19:49:03 2026] drbd p11 lima-dv-3: Handshake to peer 2 successful: Agreed network protocol version 125
[Tue Nov 17 19:49:03 2026] drbd p11 lima-dv-3: Feature flags enabled on protocol level: 0x7ff TRIM THIN_RESYNC WRITE_SAME WRITE_ZEROES RESYNC_DAGTAG WRITE_POSTPONE
[Tue Nov 17 19:49:03 2026] drbd p11: Preparing remote state change 590312596: 0->2 role( Primary ) conn( Connected )
[Tue Nov 17 19:49:03 2026] drbd p11 lima-dv-1: Aborting remote state change 590312596
[Tue Nov 17 19:49:03 2026] drbd p11: Preparing cluster-wide state change 3042428756: 1->2 role( Secondary ) conn( Connected )
[Tue Nov 17 19:49:03 2026] drbd p11: Aborting cluster-wide state change 3042428756 (0ms) rv = -19
[Tue Nov 17 19:49:03 2026] drbd p11: Preparing cluster-wide state change 1767100898: 1->2 role( Secondary ) conn( Connected )
[Tue Nov 17 19:49:03 2026] drbd p11/0 drbd1011 lima-dv-3: The peer is configured to be diskless but presents Inconsistent
[Tue Nov 17 19:49:03 2026] drbd p11 lima-dv-3: conn( Connecting -> Disconnecting )
[Tue Nov 17 19:49:03 2026] drbd p11 lima-dv-3: error receiving P_STATE, e: -5 l: 0!
[Tue Nov 17 19:49:03 2026] drbd p11: Aborting cluster-wide state change 1767100898 (16ms) rv = -21
[Tue Nov 17 19:49:03 2026] drbd p11 lima-dv-3: Failure to connect: Interrupted state change (-21); retrying
[Tue Nov 17 19:49:03 2026] drbd p11 lima-dv-3: Terminating sender thread
[Tue Nov 17 19:49:03 2026] drbd p11 lima-dv-3: Starting sender thread (peer-node-id 2)
[Tue Nov 17 19:49:03 2026] drbd p11 lima-dv-3: Connection closed
[Tue Nov 17 19:49:03 2026] drbd p11 lima-dv-3: conn( Disconnecting -> StandAlone ) [disconnected]
[Tue Nov 17 19:49:03 2026] drbd p11 lima-dv-3: Terminating receiver thread
[Tue Nov 17 19:49:06 2026] drbd p11: Preparing remote state change 1875635909: 0->2 role( Primary ) conn( Connected )
[Tue Nov 17 19:49:06 2026] drbd p11 lima-dv-1: Committing remote state change 1875635909 (primary_nodes=1)
[Tue Nov 17 19:49:06 2026] drbd p11/0 drbd1011 lima-dv-3: Former intentional diskless peer got bitmap slot 1
[Tue Nov 17 19:49:06 2026] drbd p11 lima-dv-3: conn( StandAlone -> Unconnected ) [connect]
[Tue Nov 17 19:49:06 2026] drbd p11 lima-dv-3: Starting receiver thread (peer-node-id 2)
[Tue Nov 17 19:49:06 2026] drbd p11 lima-dv-3: conn( Unconnected -> Connecting ) [connecting]
[Tue Nov 17 19:49:07 2026] drbd p11 lima-dv-3: Handshake to peer 2 successful: Agreed network protocol version 125
[Tue Nov 17 19:49:07 2026] drbd p11 lima-dv-3: Feature flags enabled on protocol level: 0x7ff TRIM THIN_RESYNC WRITE_SAME WRITE_ZEROES RESYNC_DAGTAG WRITE_POSTPONE
[Tue Nov 17 19:49:07 2026] drbd p11: Preparing cluster-wide state change 1944363446: 1->2 role( Secondary ) conn( Connected )
[Tue Nov 17 19:49:07 2026] drbd p11/0 drbd1011 lima-dv-3: drbd_sync_handshake:
[Tue Nov 17 19:49:07 2026] drbd p11/0 drbd1011 lima-dv-3: self 3F9C373A6A3CE662:0000000000000000:0000000000000000:0000000000000000 bits:8445 flags:100
[Tue Nov 17 19:49:07 2026] drbd p11/0 drbd1011 lima-dv-3: peer 3F9C373A6A3CE662:B765FD18B0608F6D:0000000000000000:0000000000000000 bits:245695 flags:1804
[Tue Nov 17 19:49:07 2026] drbd p11/0 drbd1011 lima-dv-3: uuid_compare()=no-sync by rule=lost-quorum
[Tue Nov 17 19:49:07 2026] drbd p11/0 drbd1011 lima-dv-3: strategy = source-set-bitmap due to disk states. (UpToDate/Inconsistent)
[Tue Nov 17 19:49:07 2026] drbd p11/0 drbd1011 lima-dv-3: Setting and writing one bitmap slot, after drbd_sync_handshake
[Tue Nov 17 19:49:07 2026] drbd p11/0 drbd1011 lima-dv-3: updated bitmap UUID 3F9C373A6A3CE662:3F9C373A6A3CE662:0000000000000000:0000000000000000
[Tue Nov 17 19:49:07 2026] drbd p11: State change 1944363446: primary_nodes=1, weak_nodes=FFFFFFFFFFFFFFF8
[Tue Nov 17 19:49:07 2026] drbd p11: Committing cluster-wide state change 1944363446 (20ms)
[Tue Nov 17 19:49:07 2026] drbd p11 lima-dv-3: conn( Connecting -> Connected ) peer( Unknown -> Secondary ) [connected]
[Tue Nov 17 19:49:07 2026] drbd p11/0 drbd1011 lima-dv-3: pdsk( DUnknown -> Inconsistent ) repl( Off -> WFBitMapS ) resync-susp( no -> peer ) replication( yes -> no ) [connected]
[Tue Nov 17 19:49:07 2026] drbd p11/0 drbd1011 lima-dv-3: send bitmap stats [Bytes(packets)]: plain 0(0), RLE 21(1), total 21; compression: 100.0%
[Tue Nov 17 19:49:07 2026] drbd p11/0 drbd1011 lima-dv-3: receive bitmap stats [Bytes(packets)]: plain 0(0), RLE 21(1), total 21; compression: 100.0%
[Tue Nov 17 19:49:07 2026] drbd p11/0 drbd1011 lima-dv-3: repl( WFBitMapS -> PausedSyncS ) [receive-bitmap]
[Tue Nov 17 19:49:07 2026] drbd p11/0 drbd1011 lima-dv-3: Began resync as PausedSyncS (will sync 1048316 KB [262079 bits set]).
[Tue Nov 17 19:49:18 2026] drbd p11/0 drbd1011: md_sync_timer expired! Worker calls drbd_md_sync().
[Tue Nov 17 19:50:29 2026] drbd p11/0 drbd1011 lima-dv-3: replication( no -> yes ) [enable-replication]
[Tue Nov 17 19:50:30 2026] drbd p11/0 drbd1011 lima-dv-3: updated UUIDs 3F9C373A6A3CE662:0000000000000000:0000000000000000:0000000000000000
[Tue Nov 17 19:50:30 2026] drbd p11/0 drbd1011 lima-dv-3: Resync done (total 82 sec; paused 0 sec; 12932 K/sec)
[Tue Nov 17 19:50:30 2026] drbd p11/0 drbd1011 lima-dv-3: expected n_oos:18166 to be 0
[Tue Nov 17 19:50:30 2026] drbd p11/0 drbd1011 lima-dv-3: pdsk( Inconsistent -> UpToDate ) repl( PausedSyncS -> Established ) [resync-finished]
[Tue Nov 17 19:50:30 2026] drbd p11/0 drbd1011 lima-dv-3: resync-susp( peer -> no ) [peer-state]
[Tue Nov 17 19:50:40 2026] drbd p11: Preparing remote state change 1203004661: 0->all role( Secondary )
[Tue Nov 17 19:50:40 2026] drbd p11 lima-dv-1: Committing remote state change 1203004661 (primary_nodes=0)
[Tue Nov 17 19:50:40 2026] drbd p11 lima-dv-1: peer( Primary -> Secondary ) [remote]
[Tue Nov 17 19:50:40 2026] drbd p11/0 drbd1011 lima-dv-3: strategy = source-use-bitmap to reconcile equal-UUID peers holding out-of-sync bits
[Tue Nov 17 19:50:40 2026] drbd p11/0 drbd1011 lima-dv-3: Becoming WFBitMapS after unstable
[Tue Nov 17 19:50:40 2026] drbd p11/0 drbd1011 lima-dv-3: pdsk( UpToDate -> Consistent ) repl( Established -> WFBitMapS ) [after-unstable]
[Tue Nov 17 19:50:40 2026] drbd p11/0 drbd1011 lima-dv-3: pdsk( Consistent -> Outdated ) [peer-state]
[Tue Nov 17 19:50:40 2026] drbd p11/0 drbd1011 lima-dv-3: send bitmap stats [Bytes(packets)]: plain 0(0), RLE 27(1), total 27; compression: 100.0%
[Tue Nov 17 19:50:40 2026] drbd p11/0 drbd1011 lima-dv-3: receive bitmap stats [Bytes(packets)]: plain 0(0), RLE 27(1), total 27; compression: 100.0%
[Tue Nov 17 19:50:40 2026] drbd p11/0 drbd1011 lima-dv-3: pdsk( Outdated -> Inconsistent ) repl( WFBitMapS -> SyncSource ) replication( yes -> no ) [receive-bitmap]
[Tue Nov 17 19:50:40 2026] drbd p11/0 drbd1011 lima-dv-3: Began resync as SyncSource (will sync 69632 KB [17408 bits set]).
--- dmesg -T | grep 'drbd p11' | tail -n 80 [dv-3]
[Tue Nov 17 19:49:03 2026] drbd p11 lima-dv-1: Connection closed
[Tue Nov 17 19:49:03 2026] drbd p11 lima-dv-1: conn( NetworkFailure -> Unconnected ) [disconnected]
[Tue Nov 17 19:49:03 2026] drbd p11 lima-dv-1: Restarting receiver thread
[Tue Nov 17 19:49:03 2026] drbd p11 lima-dv-1: conn( Unconnected -> Connecting ) [connecting]
[Tue Nov 17 19:49:03 2026] drbd p11: Preparing remote state change 1767100898: 1->2 role( Secondary ) conn( Connected )
[Tue Nov 17 19:49:03 2026] drbd p11/0 drbd1011 lima-dv-2: drbd_sync_handshake:
[Tue Nov 17 19:49:03 2026] drbd p11/0 drbd1011 lima-dv-2: self 0000000000000004:0000000000000000:0000000000000000:0000000000000000 bits:262079 flags:2024
[Tue Nov 17 19:49:03 2026] drbd p11/0 drbd1011 lima-dv-2: peer 3F9C373A6A3CE662:0000000000000000:0000000000000000:0000000000000000 bits:0 flags:100
[Tue Nov 17 19:49:03 2026] drbd p11/0 drbd1011 lima-dv-2: uuid_compare()=target-set-bitmap by rule=just-created-self
[Tue Nov 17 19:49:03 2026] drbd p11/0 drbd1011 lima-dv-2: Setting and writing the whole bitmap, fresh node
[Tue Nov 17 19:49:03 2026] drbd p11 lima-dv-2: meta connection shut down by peer.
[Tue Nov 17 19:49:03 2026] drbd p11 lima-dv-2: conn( Connecting -> NetworkFailure )
[Tue Nov 17 19:49:03 2026] drbd p11 lima-dv-2: meta connection shut down by peer.
[Tue Nov 17 19:49:03 2026] drbd p11 lima-dv-2: sock was shut down by peer
[Tue Nov 17 19:49:03 2026] drbd p11 lima-dv-2: Terminating sender thread
[Tue Nov 17 19:49:03 2026] drbd p11 lima-dv-2: Starting sender thread (peer-node-id 1)
[Tue Nov 17 19:49:03 2026] drbd p11 lima-dv-2: Aborting remote state change 1767100898 commit not possible
[Tue Nov 17 19:49:03 2026] drbd p11 lima-dv-2: Connection closed
[Tue Nov 17 19:49:03 2026] drbd p11 lima-dv-2: conn( NetworkFailure -> Unconnected ) [disconnected]
[Tue Nov 17 19:49:03 2026] drbd p11 lima-dv-2: Restarting receiver thread
[Tue Nov 17 19:49:03 2026] drbd p11 lima-dv-2: conn( Unconnected -> Connecting ) [connecting]
[Tue Nov 17 19:49:06 2026] drbd p11 lima-dv-1: Handshake to peer 0 successful: Agreed network protocol version 125
[Tue Nov 17 19:49:06 2026] drbd p11 lima-dv-1: Feature flags enabled on protocol level: 0x7ff TRIM THIN_RESYNC WRITE_SAME WRITE_ZEROES RESYNC_DAGTAG WRITE_POSTPONE
[Tue Nov 17 19:49:06 2026] drbd p11: Preparing remote state change 1875635909: 0->2 role( Primary ) conn( Connected )
[Tue Nov 17 19:49:06 2026] drbd p11/0 drbd1011 lima-dv-1: drbd_sync_handshake:
[Tue Nov 17 19:49:06 2026] drbd p11/0 drbd1011 lima-dv-1: self 0000000000000004:0000000000000000:0000000000000000:0000000000000000 bits:262079 flags:2024
[Tue Nov 17 19:49:06 2026] drbd p11/0 drbd1011 lima-dv-1: peer 3F9C373A6A3CE662:0000000000000000:0000000000000000:0000000000000000 bits:8445 flags:1120
[Tue Nov 17 19:49:06 2026] drbd p11/0 drbd1011 lima-dv-1: uuid_compare()=target-set-bitmap by rule=just-created-self
[Tue Nov 17 19:49:06 2026] drbd p11/0 drbd1011 lima-dv-1: Setting and writing the whole bitmap, fresh node
[Tue Nov 17 19:49:06 2026] drbd p11 lima-dv-1: Committing remote state change 1875635909 (primary_nodes=1)
[Tue Nov 17 19:49:06 2026] drbd p11 lima-dv-1: conn( Connecting -> Connected ) peer( Unknown -> Primary ) [remote]
[Tue Nov 17 19:49:06 2026] drbd p11/0 drbd1011: quorum( no -> yes ) [remote]
[Tue Nov 17 19:49:06 2026] drbd p11/0 drbd1011 lima-dv-1: pdsk( DUnknown -> UpToDate ) repl( Off -> WFBitMapT ) peer_replication( yes -> no ) [remote]
[Tue Nov 17 19:49:06 2026] drbd p11/0 drbd1011: Resumed AL updates
[Tue Nov 17 19:49:06 2026] drbd p11/0 drbd1011: Setting exposed data uuid: 3F9C373A6A3CE662
[Tue Nov 17 19:49:06 2026] drbd p11/0 drbd1011: persisting effective size = 1024 MB (1048316 KB)
[Tue Nov 17 19:49:06 2026] drbd p11/0 drbd1011 lima-dv-1: receive bitmap stats [Bytes(packets)]: plain 0(0), RLE 21(1), total 21; compression: 100.0%
[Tue Nov 17 19:49:06 2026] drbd p11/0 drbd1011 lima-dv-1: send bitmap stats [Bytes(packets)]: plain 0(0), RLE 21(1), total 21; compression: 100.0%
[Tue Nov 17 19:49:06 2026] drbd p11/0 drbd1011 lima-dv-2: resync-susp( no -> connection dependency ) [receive-bitmap]
[Tue Nov 17 19:49:06 2026] drbd p11/0 drbd1011 lima-dv-1: repl( WFBitMapT -> SyncTarget ) [receive-bitmap]
[Tue Nov 17 19:49:06 2026] drbd p11/0 drbd1011 lima-dv-1: Began resync as SyncTarget (will sync 1048316 KB [262079 bits set]).
[Tue Nov 17 19:49:06 2026] drbd p11/0 drbd1011: Bitmap UUID for initial resync: B765FD18B0608F6D
[Tue Nov 17 19:49:07 2026] drbd p11 lima-dv-2: Handshake to peer 1 successful: Agreed network protocol version 125
[Tue Nov 17 19:49:07 2026] drbd p11 lima-dv-2: Feature flags enabled on protocol level: 0x7ff TRIM THIN_RESYNC WRITE_SAME WRITE_ZEROES RESYNC_DAGTAG WRITE_POSTPONE
[Tue Nov 17 19:49:07 2026] drbd p11: Preparing remote state change 1944363446: 1->2 role( Secondary ) conn( Connected )
[Tue Nov 17 19:49:07 2026] drbd p11/0 drbd1011 lima-dv-2: drbd_sync_handshake:
[Tue Nov 17 19:49:07 2026] drbd p11/0 drbd1011 lima-dv-2: self 3F9C373A6A3CE662:B765FD18B0608F6D:0000000000000000:0000000000000000 bits:245695 flags:4
[Tue Nov 17 19:49:07 2026] drbd p11/0 drbd1011 lima-dv-2: peer 3F9C373A6A3CE662:0000000000000000:0000000000000000:0000000000000000 bits:8445 flags:100
[Tue Nov 17 19:49:07 2026] drbd p11/0 drbd1011 lima-dv-2: uuid_compare()=no-sync by rule=lost-quorum
[Tue Nov 17 19:49:07 2026] drbd p11/0 drbd1011 lima-dv-2: strategy = target-set-bitmap due to disk states. (Inconsistent/UpToDate)
[Tue Nov 17 19:49:07 2026] drbd p11 lima-dv-2: Committing remote state change 1944363446 (primary_nodes=1)
[Tue Nov 17 19:49:07 2026] drbd p11 lima-dv-2: conn( Connecting -> Connected ) peer( Unknown -> Secondary ) [remote]
[Tue Nov 17 19:49:07 2026] drbd p11/0 drbd1011 lima-dv-2: pdsk( DUnknown -> UpToDate ) repl( Off -> WFBitMapT ) peer_replication( yes -> no ) [remote]
[Tue Nov 17 19:49:07 2026] drbd p11/0 drbd1011 lima-dv-2: receive bitmap stats [Bytes(packets)]: plain 0(0), RLE 21(1), total 21; compression: 100.0%
[Tue Nov 17 19:49:07 2026] drbd p11/0 drbd1011 lima-dv-2: send bitmap stats [Bytes(packets)]: plain 0(0), RLE 21(1), total 21; compression: 100.0%
[Tue Nov 17 19:49:07 2026] drbd p11/0 drbd1011 lima-dv-2: repl( WFBitMapT -> PausedSyncT ) [receive-bitmap]
[Tue Nov 17 19:49:07 2026] drbd p11/0 drbd1011 lima-dv-2: Began resync as PausedSyncT (will sync 1048316 KB [262079 bits set]).
[Tue Nov 17 19:49:07 2026] drbd p11/0 drbd1011: Bitmap UUID for initial resync: B723253920811A9A
[Tue Nov 17 19:49:18 2026] drbd p11/0 drbd1011: md_sync_timer expired! Worker calls drbd_md_sync().
[Tue Nov 17 19:50:29 2026] drbd p11/0 drbd1011 lima-dv-2: peer_replication( no -> yes ) [enable-peer-replication]
[Tue Nov 17 19:50:29 2026] drbd p11/0 drbd1011 lima-dv-1: peer_replication( no -> yes ) [enable-peer-replication]
[Tue Nov 17 19:50:30 2026] drbd p11/0 drbd1011 lima-dv-1: Resync done (total 84 sec; paused 0 sec; 12728 K/sec)
[Tue Nov 17 19:50:30 2026] drbd p11/0 drbd1011 lima-dv-1: updated UUIDs 3F9C373A6A3CE662:0000000000000000:1AF6148E57588A3E:0000000000000000
[Tue Nov 17 19:50:30 2026] drbd p11/0 drbd1011: disk( Inconsistent -> UpToDate ) [resync-finished]
[Tue Nov 17 19:50:30 2026] drbd p11/0 drbd1011 lima-dv-2: repl( PausedSyncT -> Established ) resync-susp( connection dependency -> no ) [resync-finished]
[Tue Nov 17 19:50:30 2026] drbd p11/0 drbd1011 lima-dv-1: repl( SyncTarget -> Established ) [resync-finished]
[Tue Nov 17 19:50:30 2026] drbd p11/0 drbd1011 lima-dv-1: Sync source can not get this write, postponing it towards the writer
[Tue Nov 17 19:50:40 2026] drbd p11: Preparing remote state change 1203004661: 0->all role( Secondary )
[Tue Nov 17 19:50:40 2026] drbd p11 lima-dv-1: Committing remote state change 1203004661 (primary_nodes=0)
[Tue Nov 17 19:50:40 2026] drbd p11 lima-dv-1: peer( Primary -> Secondary ) [remote]
[Tue Nov 17 19:50:40 2026] drbd p11/0 drbd1011 lima-dv-2: strategy = target-use-bitmap to reconcile equal-UUID peers holding out-of-sync bits
[Tue Nov 17 19:50:40 2026] drbd p11/0 drbd1011 lima-dv-2: Becoming WFBitMapT after unstable
[Tue Nov 17 19:50:40 2026] drbd p11/0 drbd1011: disk( UpToDate -> Outdated ) [after-unstable]
[Tue Nov 17 19:50:40 2026] drbd p11/0 drbd1011 lima-dv-2: repl( Established -> WFBitMapT ) [after-unstable]
[Tue Nov 17 19:50:40 2026] drbd p11/0 drbd1011 lima-dv-2: receive bitmap stats [Bytes(packets)]: plain 0(0), RLE 27(1), total 27; compression: 100.0%
[Tue Nov 17 19:50:40 2026] drbd p11/0 drbd1011 lima-dv-2: send bitmap stats [Bytes(packets)]: plain 0(0), RLE 27(1), total 27; compression: 100.0%
[Tue Nov 17 19:50:40 2026] drbd p11/0 drbd1011: disk( Outdated -> Inconsistent ) [receive-bitmap]
[Tue Nov 17 19:50:40 2026] drbd p11/0 drbd1011 lima-dv-2: repl( WFBitMapT -> SyncTarget ) peer_replication( yes -> no ) [receive-bitmap]
[Tue Nov 17 19:50:40 2026] drbd p11/0 drbd1011 lima-dv-1: resync-susp( no -> connection dependency ) peer_replication( yes -> no ) [receive-bitmap]
[Tue Nov 17 19:50:40 2026] drbd p11/0 drbd1011 lima-dv-2: Began resync as SyncTarget (will sync 69632 KB [17408 bits set]).
P11 run 1 result: dv-3 UpToDate=1

### P11 run 2: adjust on dv-1 and dv-2 first, then dv-3 changes
$ [dv-1] drbdadm create-md --force --max-peers=7 p11
initializing bitmap (224 KB) to all zero
initializing activity log
Writing meta data...
New drbd meta data block successfully created.
rc=0
$ [dv-2] drbdadm create-md --force --max-peers=7 p11
initializing bitmap (224 KB) to all zero
initializing activity log
Writing meta data...
New drbd meta data block successfully created.
rc=0
$ [dv-1] drbdadm up p11
rc=0
$ [dv-2] drbdadm up p11
rc=0
$ [dv-3] drbdadm up p11
rc=0
wait: dv-1: 2 connections Connected after 2 s at most
wait: dv-2: 2 connections Connected after 2 s at most
wait: dv-3: 2 connections Connected after 2 s at most
$ [dv-1] drbdadm new-current-uuid --clear-bitmap p11/0
rc=0
$ [dv-1] drbdsetup status p11 --verbose
p11 node-id:0 role:Secondary suspended:no force-io-failures:no
  volume:0 minor:1011 disk:UpToDate backing_dev:/dev/ambar-exp/p11 quorum:yes
      open:no blocked:no
  lima-dv-2 node-id:1 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
  lima-dv-3 node-id:2 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:Diskless peer-client:yes
        resync-suspended:no

rc=0
$ [dv-1] mkfs.ext4 -q -F -E nodiscard /dev/drbd1011
rc=0
19:51:48.737
2199
started

### P11 run 2, step 1: p11.res with a disk on dv-3 on dv-1 and dv-2, adjust while dv-3 runs without a disk
@ 19:51:51.817 adjust on dv-1
$ [dv-1] drbdadm adjust p11
rc=0
$ [dv-2] drbdadm adjust p11
rc=0
$ [dv-1] drbdsetup status p11 --verbose --statistics
p11 node-id:0 role:Primary suspended:no force-io-failures:no
    write-ordering:flush
  volume:0 minor:1011 disk:UpToDate backing_dev:/dev/ambar-exp/p11 quorum:yes
      open:yes
      size:1048316 read:1005 written:138496 al-writes:9 bm-writes:63 upper-pending:1
      lower-pending:1 al-suspended:no blocked:no
  lima-dv-2 node-id:1 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:8 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
        received:0 sent:105604 out-of-sync:0 pending:0 unacked:0
  lima-dv-3 node-id:2 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:Diskless peer-client:no
        resync-suspended:no
        received:0 sent:0 out-of-sync:33780 pending:0 unacked:0

rc=0
$ [dv-2] drbdsetup status p11 --verbose --statistics
p11 node-id:1 role:Secondary suspended:no force-io-failures:no
    write-ordering:flush
  volume:0 minor:1011 disk:UpToDate backing_dev:/dev/ambar-exp/p11 quorum:yes
      open:no
      size:1048316 read:0 written:144960 al-writes:10 bm-writes:63 upper-pending:0
      lower-pending:0 al-suspended:no blocked:no
  lima-dv-1 node-id:0 connection:Connected role:Primary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
        received:112064 sent:0 out-of-sync:0 pending:0 unacked:0
  lima-dv-3 node-id:2 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:Diskless peer-client:no
        resync-suspended:no
        received:0 sent:0 out-of-sync:33784 pending:0 unacked:0

rc=0
$ [dv-3] drbdsetup status p11 --verbose --statistics
p11 node-id:2 role:Secondary suspended:no force-io-failures:no
    write-ordering:none
  volume:0 minor:1011 disk:Diskless client:yes backing_dev:none quorum:yes open:no
      size:1048316 read:0 written:0 al-writes:0 bm-writes:0 upper-pending:0
      lower-pending:0 al-suspended:no blocked:no
  lima-dv-1 node-id:0 connection:Connected role:Primary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
        received:0 sent:0 out-of-sync:0 pending:0 unacked:0
  lima-dv-2 node-id:1 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
        received:0 sent:0 out-of-sync:0 pending:0 unacked:0

rc=0
@ 19:52:02.661 state after adjust

### P11 run 2, step 2: down on dv-3, status of dv-1 and dv-2
@ 19:52:02.779 down on dv-3
$ [dv-3] drbdadm down p11
rc=0
$ [dv-1] drbdsetup status p11 --verbose
p11 node-id:0 role:Primary suspended:no force-io-failures:no
  volume:0 minor:1011 disk:UpToDate backing_dev:/dev/ambar-exp/p11 quorum:yes
      open:yes blocked:no
  lima-dv-2 node-id:1 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
  lima-dv-3 node-id:2 connection:Connecting role:Unknown tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Off peer-disk:DUnknown resync-suspended:no

rc=0
$ [dv-2] drbdsetup status p11 --verbose
p11 node-id:1 role:Secondary suspended:no force-io-failures:no
  volume:0 minor:1011 disk:UpToDate backing_dev:/dev/ambar-exp/p11 quorum:yes
      open:no blocked:no
  lima-dv-1 node-id:0 connection:Connected role:Primary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
  lima-dv-3 node-id:2 connection:Connecting role:Unknown tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Off peer-disk:DUnknown resync-suspended:no

rc=0

### P11 run 2, step 3: create-md and up on dv-3
@ 19:52:10.096 create-md on dv-3 after down
$ [dv-3] drbdadm create-md --force --max-peers=7 p11
initializing bitmap (224 KB) to all zero
initializing activity log
Writing meta data...
New drbd meta data block successfully created.
rc=0
$ [dv-3] drbdadm up p11
rc=0
@ 19:52:12.630 up on dv-3 (UP_T0)
$ [dv-1] drbdsetup status p11 --verbose --statistics
p11 node-id:0 role:Primary suspended:no force-io-failures:no
    write-ordering:flush
  volume:0 minor:1011 disk:UpToDate backing_dev:/dev/ambar-exp/p11 quorum:yes
      open:yes
      size:1048316 read:10733 written:231184 al-writes:9 bm-writes:122 upper-pending:1
      lower-pending:1 al-suspended:no blocked:no
  lima-dv-2 node-id:1 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:8 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
        received:0 sent:198288 out-of-sync:0 pending:1 unacked:0
  lima-dv-3 node-id:2 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:SyncSource peer-disk:Inconsistent done:0.93
        resync-suspended:no
        received:0 sent:9728 out-of-sync:1038600 pending:0 unacked:1 dbdt1:2.66 eta:381

rc=0
$ [dv-2] drbdsetup status p11 --verbose --statistics
p11 node-id:1 role:Secondary suspended:no force-io-failures:no
    write-ordering:flush
  volume:0 minor:1011 disk:UpToDate backing_dev:/dev/ambar-exp/p11 quorum:yes
      open:no
      size:1048316 read:0 written:238940 al-writes:11 bm-writes:121 upper-pending:0
      lower-pending:1 al-suspended:no blocked:no
  lima-dv-1 node-id:0 connection:Connected role:Primary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
        received:206048 sent:0 out-of-sync:0 pending:0 unacked:1
  lima-dv-3 node-id:2 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:PausedSyncS peer-disk:Inconsistent done:0.78
        resync-suspended:peer
        received:0 sent:0 out-of-sync:1040136 pending:0 unacked:0 dbdt1:1.79 eta:568

rc=0
$ [dv-3] drbdsetup status p11 --verbose --statistics
p11 node-id:2 role:Secondary suspended:no force-io-failures:no
    write-ordering:flush
  volume:0 minor:1011 disk:Inconsistent backing_dev:/dev/ambar-exp/p11 quorum:yes
      open:no
      size:1048316 read:0 written:10248 al-writes:0 bm-writes:58 upper-pending:0
      lower-pending:0 al-suspended:no blocked:no
  lima-dv-1 node-id:0 connection:Connected role:Primary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:SyncTarget peer-disk:UpToDate done:0.98 resync-suspended:no
        received:10248 sent:0 out-of-sync:1038080 pending:5 unacked:0 dbdt1:1.79 eta:566
  lima-dv-2 node-id:1 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:PausedSyncT peer-disk:UpToDate done:0.78
        resync-suspended:dependency
        received:0 sent:0 out-of-sync:1040136 pending:0 unacked:0 dbdt1:1.45 eta:699

rc=0

### P11 run 2, step 5: wait for UpToDate on dv-3
t+7 s [dv-3]
  volume:0 minor:1011 disk:Inconsistent backing_dev:/dev/ambar-exp/p11 quorum:yes
  volume:0 replication:SyncTarget peer-disk:UpToDate done:3.32 resync-suspended:no
  received:34816 sent:0 out-of-sync:1013512 pending:0 unacked:0 dbdt1:7.17 eta:138
  volume:0 replication:PausedSyncT peer-disk:UpToDate done:3.13
  received:0 sent:0 out-of-sync:1015560 pending:0 unacked:0 dbdt1:4.96 eta:200
t+13 s [dv-3]
  volume:0 minor:1011 disk:Inconsistent backing_dev:/dev/ambar-exp/p11 quorum:yes
  volume:0 replication:SyncTarget peer-disk:UpToDate done:6.54 resync-suspended:no
  received:68528 sent:0 out-of-sync:979800 pending:7 unacked:0 dbdt1:0.92 eta:1040
  volume:0 replication:PausedSyncT peer-disk:UpToDate done:6.25
  received:0 sent:0 out-of-sync:982792 pending:0 unacked:0 dbdt1:5.02 eta:191
t+19 s [dv-3]
  volume:0 minor:1011 disk:Inconsistent backing_dev:/dev/ambar-exp/p11 quorum:yes
  volume:0 replication:SyncTarget peer-disk:UpToDate done:10.96 resync-suspended:no
  received:114880 sent:0 out-of-sync:933448 pending:6 unacked:0 dbdt1:12.01 eta:76
  volume:0 replication:PausedSyncT peer-disk:UpToDate done:10.94
  received:0 sent:0 out-of-sync:933640 pending:0 unacked:0 dbdt1:5.95 eta:153
t+26 s [dv-3]
  volume:0 minor:1011 disk:Inconsistent backing_dev:/dev/ambar-exp/p11 quorum:yes
  volume:0 replication:SyncTarget peer-disk:UpToDate done:11.30 resync-suspended:no
  received:118480 sent:0 out-of-sync:929848 pending:3 unacked:0 dbdt1:0.24 eta:3735
  volume:0 replication:PausedSyncT peer-disk:UpToDate done:10.94
  received:0 sent:0 out-of-sync:933640 pending:0 unacked:0 dbdt1:4.47 eta:204
t+32 s [dv-3]
  volume:0 minor:1011 disk:Inconsistent backing_dev:/dev/ambar-exp/p11 quorum:yes
  volume:0 replication:SyncTarget peer-disk:UpToDate done:27.04 resync-suspended:no
  received:283648 sent:0 out-of-sync:765336 pending:8 unacked:0 dbdt1:17.58 eta:43
  volume:0 replication:PausedSyncT peer-disk:UpToDate done:26.57
  received:0 sent:0 out-of-sync:769808 pending:0 unacked:0 dbdt1:8.66 eta:87
t+38 s [dv-3]
  volume:0 minor:1011 disk:Inconsistent backing_dev:/dev/ambar-exp/p11 quorum:yes
  volume:0 replication:SyncTarget peer-disk:UpToDate done:42.41 resync-suspended:no
  received:451584 sent:0 out-of-sync:613132 pending:8 unacked:0 dbdt1:16.00 eta:37
  volume:0 replication:PausedSyncT peer-disk:UpToDate done:42.09
  received:0 sent:0 out-of-sync:614156 pending:0 unacked:0 dbdt1:11.22 eta:53
t+45 s [dv-3]
  volume:0 minor:1011 disk:Inconsistent backing_dev:/dev/ambar-exp/p11 quorum:yes
  volume:0 replication:SyncTarget peer-disk:UpToDate done:68.57 resync-suspended:no
  received:730112 sent:0 out-of-sync:334616 pending:8 unacked:0 dbdt1:42.86 eta:8
  volume:0 replication:PausedSyncT peer-disk:UpToDate done:67.97
  received:0 sent:0 out-of-sync:339724 pending:0 unacked:0 dbdt1:15.74 eta:21
t+51 s [dv-3]
  volume:0 minor:1011 disk:Inconsistent backing_dev:/dev/ambar-exp/p11 quorum:yes
  volume:0 replication:SyncTarget peer-disk:UpToDate done:72.50 resync-suspended:no
  received:771904 sent:0 out-of-sync:292840 pending:10 unacked:0 dbdt1:1.91 eta:150
  volume:0 replication:PausedSyncT peer-disk:UpToDate done:71.83
  received:0 sent:0 out-of-sync:298764 pending:0 unacked:0 dbdt1:14.62 eta:20
t+57 s [dv-3]
  volume:0 minor:1011 disk:Inconsistent backing_dev:/dev/ambar-exp/p11 quorum:yes
  volume:0 replication:SyncTarget peer-disk:UpToDate done:80.69 resync-suspended:no
  received:859136 sent:0 out-of-sync:205620 pending:0 unacked:0 dbdt1:18.15 eta:11
  volume:0 replication:PausedSyncT peer-disk:UpToDate done:79.94
  received:0 sent:0 out-of-sync:212748 pending:0 unacked:0 dbdt1:14.51 eta:14
t+63 s [dv-3]
  volume:0 minor:1011 disk:Inconsistent backing_dev:/dev/ambar-exp/p11 quorum:yes
  volume:0 replication:SyncTarget peer-disk:UpToDate done:98.65 resync-suspended:no
  received:1050380 sent:0 out-of-sync:14396 pending:1 unacked:0 dbdt1:27.08 eta:1
  volume:0 replication:PausedSyncT peer-disk:UpToDate done:98.07
  received:0 sent:0 out-of-sync:20480 pending:0 unacked:0 dbdt1:16.09 eta:1
t+69 s [dv-3]
  volume:0 minor:1011 disk:Inconsistent backing_dev:/dev/ambar-exp/p11 quorum:yes
  volume:0 replication:SyncTarget peer-disk:UpToDate done:99.95 resync-suspended:no
  received:1085524 sent:0 out-of-sync:532 pending:0 unacked:0 dbdt1:0.52 eta:1
  volume:0 replication:PausedSyncT peer-disk:UpToDate done:99.31
  received:0 sent:0 out-of-sync:7408 pending:0 unacked:0 dbdt1:14.81 eta:0
t+75 s [dv-3]
  volume:0 minor:1011 disk:UpToDate backing_dev:/dev/ambar-exp/p11 quorum:yes
  volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
  received:1112424 sent:0 out-of-sync:0 pending:0 unacked:0
  volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
  received:0 sent:0 out-of-sync:0 pending:0 unacked:0
UpToDate on dv-3: t+75 s after the mark (poll step 5 s plus the call latency)
$ [dv-1] drbdsetup status p11 --verbose --statistics
p11 node-id:0 role:Primary suspended:no force-io-failures:no
    write-ordering:flush
  volume:0 minor:1011 disk:UpToDate backing_dev:/dev/ambar-exp/p11 quorum:yes
      open:yes
      size:1048316 read:1081565 written:674636 al-writes:10 bm-writes:216
      upper-pending:2 lower-pending:0 al-suspended:no blocked:no
  lima-dv-2 node-id:1 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:16 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
        received:0 sent:641740 out-of-sync:0 pending:2 unacked:0
  lima-dv-3 node-id:2 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:16 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
        received:0 sent:1132460 out-of-sync:4 pending:2 unacked:0

rc=0
$ [dv-2] drbdsetup status p11 --verbose --statistics
p11 node-id:1 role:Secondary suspended:no force-io-failures:no
    write-ordering:flush
  volume:0 minor:1011 disk:UpToDate backing_dev:/dev/ambar-exp/p11 quorum:yes
      open:no
      size:1048316 read:0 written:678576 al-writes:11 bm-writes:203 upper-pending:0
      lower-pending:1 al-suspended:no blocked:no
  lima-dv-1 node-id:0 connection:Connected role:Primary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
        received:645680 sent:0 out-of-sync:0 pending:0 unacked:0
  lima-dv-3 node-id:2 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
        received:0 sent:0 out-of-sync:0 pending:0 unacked:0

rc=0
$ [dv-3] drbdsetup status p11 --verbose --statistics
p11 node-id:2 role:Secondary suspended:no force-io-failures:no
    write-ordering:flush
  volume:0 minor:1011 disk:UpToDate backing_dev:/dev/ambar-exp/p11 quorum:yes
      open:no
      size:1048316 read:0 written:1140208 al-writes:6 bm-writes:178 upper-pending:0
      lower-pending:1 al-suspended:no blocked:no
  lima-dv-1 node-id:0 connection:Connected role:Primary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
        received:1140212 sent:0 out-of-sync:0 pending:0 unacked:1
  lima-dv-2 node-id:1 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
        received:0 sent:0 out-of-sync:0 pending:0 unacked:0

rc=0

### P11 run 2, step 6: stop the writer, quorum log, kernel logs
@ 19:53:35.432 stop the writer
acked=40508 on-volume=40509
--- probe.sh log (stdout and stderr)
--- watch.sh log
samples=219 probe=alive:R=15 probe=alive:S=204 longest-gap-without-new-acked=0.6s (from 19:52:07.784)
--- quorum log of dv-1 (runs of equal values: first - last  value  samples)
19:51:50.194 - 19:53:36.800  "quorum": true  x107
--- dmesg -T | grep 'drbd p11' | tail -n 80 [dv-1]
[Tue Nov 17 19:51:28 2026] drbd p11/0 drbd1011 lima-dv-2: pdsk( DUnknown -> Inconsistent ) repl( Off -> Established ) replication( yes -> no ) peer_replication( yes -> no ) [connected]
[Tue Nov 17 19:51:28 2026] drbd p11/0 drbd1011: Resumed AL updates
[Tue Nov 17 19:51:29 2026] drbd p11 lima-dv-3: Handshake to peer 2 successful: Agreed network protocol version 125
[Tue Nov 17 19:51:29 2026] drbd p11 lima-dv-3: Feature flags enabled on protocol level: 0x7ff TRIM THIN_RESYNC WRITE_SAME WRITE_ZEROES RESYNC_DAGTAG WRITE_POSTPONE
[Tue Nov 17 19:51:29 2026] drbd p11: Preparing cluster-wide state change 763770148: 0->2 role( Secondary ) conn( Connected )
[Tue Nov 17 19:51:29 2026] drbd p11/0 drbd1011 lima-dv-3: self 0000000000000004:0000000000000000:0000000000000000:0000000000000000 bits:0 flags:0
[Tue Nov 17 19:51:29 2026] drbd p11/0 drbd1011 lima-dv-3: peer's exposed UUID: 0000000000000000
[Tue Nov 17 19:51:29 2026] drbd p11: State change 763770148: primary_nodes=0, weak_nodes=0
[Tue Nov 17 19:51:29 2026] drbd p11: Committing cluster-wide state change 763770148 (20ms)
[Tue Nov 17 19:51:29 2026] drbd p11 lima-dv-3: conn( Connecting -> Connected ) peer( Unknown -> Secondary ) [connected]
[Tue Nov 17 19:51:29 2026] drbd p11/0 drbd1011 lima-dv-3: pdsk( DUnknown -> Diskless ) repl( Off -> Established ) peer_replication( yes -> no ) [connected]
[Tue Nov 17 19:51:29 2026] drbd p11: Preparing remote state change 3926141265: 1->2 role( Secondary ) conn( Connected )
[Tue Nov 17 19:51:29 2026] drbd p11 lima-dv-2: Committing remote state change 3926141265 (primary_nodes=0)
[Tue Nov 17 19:51:36 2026] drbd p11/0 drbd1011 lima-dv-2: Preparing to skip initial sync
[Tue Nov 17 19:51:36 2026] drbd p11/0 drbd1011: Bitmap UUID for initial resync: E0B8AA77D93AE54C
[Tue Nov 17 19:51:36 2026] drbd p11/0 drbd1011: new current UUID: 92A73866FF86CC7E weak: FFFFFFFFFFFFFFFD
[Tue Nov 17 19:51:36 2026] drbd p11/0 drbd1011 lima-dv-3: cleared bitmap UUID 92A73866FF86CC7E:0000000000000000:0000000000000000:0000000000000000
[Tue Nov 17 19:51:36 2026] drbd p11/0 drbd1011 lima-dv-2: cleared bitmap UUID 92A73866FF86CC7E:0000000000000000:0000000000000000:0000000000000000
[Tue Nov 17 19:51:36 2026] drbd p11/0 drbd1011: disk( Inconsistent -> UpToDate ) [new-c-uuid]
[Tue Nov 17 19:51:36 2026] drbd p11/0 drbd1011 lima-dv-2: pdsk( Inconsistent -> UpToDate ) replication( no -> yes ) [new-c-uuid]
[Tue Nov 17 19:51:36 2026] drbd p11/0 drbd1011: persisting effective size = 1024 MB (1048316 KB)
[Tue Nov 17 19:51:41 2026] drbd p11: Preparing cluster-wide state change 728516217: 0->all role( Primary )
[Tue Nov 17 19:51:41 2026] drbd p11: State change 728516217: primary_nodes=1, weak_nodes=FFFFFFFFFFFFFFF8
[Tue Nov 17 19:51:41 2026] drbd p11: Committing cluster-wide state change 728516217 (0ms)
[Tue Nov 17 19:51:41 2026] drbd p11: role( Secondary -> Primary ) [mkfs.ext4:215067 auto-promote]
[Tue Nov 17 19:51:41 2026] drbd p11/0 drbd1011: gen-obligation: NONE -> ARMED(promoted)
[Tue Nov 17 19:51:41 2026] drbd p11/0 drbd1011: gen-obligation: ARMED(promoted) -> MINTING(promoted)
[Tue Nov 17 19:51:41 2026] drbd p11/0 drbd1011: gen-obligation: MINTING(promoted) -> DISCHARGED(promoted)
[Tue Nov 17 19:51:41 2026] drbd p11: Preparing cluster-wide state change 1523522869: 0->all role( Secondary )
[Tue Nov 17 19:51:41 2026] drbd p11: State change 1523522869: primary_nodes=0, weak_nodes=0
[Tue Nov 17 19:51:41 2026] drbd p11: Committing cluster-wide state change 1523522869 (0ms)
[Tue Nov 17 19:51:41 2026] drbd p11: role( Primary -> Secondary ) [mkfs.ext4:215067 auto-demote]
[Tue Nov 17 19:51:41 2026] drbd p11: Preparing cluster-wide state change 597684032: 0->all role( Primary )
[Tue Nov 17 19:51:41 2026] drbd p11: State change 597684032: primary_nodes=1, weak_nodes=FFFFFFFFFFFFFFF8
[Tue Nov 17 19:51:41 2026] drbd p11: Committing cluster-wide state change 597684032 (0ms)
[Tue Nov 17 19:51:41 2026] drbd p11: role( Secondary -> Primary ) [mount:215091 auto-promote]
[Tue Nov 17 19:51:41 2026] drbd p11/0 drbd1011: gen-obligation: DISCHARGED(promoted) -> ARMED(promoted)
[Tue Nov 17 19:51:41 2026] drbd p11/0 drbd1011: gen-obligation: ARMED(promoted) -> MINTING(promoted)
[Tue Nov 17 19:51:41 2026] drbd p11/0 drbd1011: gen-obligation: MINTING(promoted) -> DISCHARGED(promoted)
[Tue Nov 17 19:51:50 2026] drbd p11/0 drbd1011 lima-dv-3: Former intentional diskless peer got bitmap slot 1
[Tue Nov 17 19:51:57 2026] drbd p11/0 drbd1011: md_sync_timer expired! Worker calls drbd_md_sync().
[Tue Nov 17 19:52:01 2026] drbd p11: Preparing remote state change 1491530933: 2->0 conn( Disconnecting ) disk( Outdated )
[Tue Nov 17 19:52:01 2026] drbd p11 lima-dv-3: Committing remote state change 1491530933 (primary_nodes=1)
[Tue Nov 17 19:52:01 2026] drbd p11 lima-dv-3: conn( Connected -> TearDown ) peer( Secondary -> Unknown ) [remote]
[Tue Nov 17 19:52:01 2026] drbd p11/0 drbd1011 lima-dv-3: pdsk( Diskless -> DUnknown ) repl( Established -> Off ) [remote]
[Tue Nov 17 19:52:01 2026] drbd p11 lima-dv-3: Terminating sender thread
[Tue Nov 17 19:52:01 2026] drbd p11 lima-dv-3: Starting sender thread (peer-node-id 2)
[Tue Nov 17 19:52:01 2026] drbd p11 lima-dv-3: Connection closed
[Tue Nov 17 19:52:01 2026] drbd p11 lima-dv-3: conn( TearDown -> Unconnected ) [disconnected]
[Tue Nov 17 19:52:01 2026] drbd p11 lima-dv-3: Restarting receiver thread
[Tue Nov 17 19:52:01 2026] drbd p11 lima-dv-3: conn( Unconnected -> Connecting ) [connecting]
[Tue Nov 17 19:52:01 2026] drbd p11: Preparing remote state change 1079172654: 2->1 conn( Disconnecting )
[Tue Nov 17 19:52:01 2026] drbd p11 lima-dv-2: Committing remote state change 1079172654 (primary_nodes=1)
[Tue Nov 17 19:52:10 2026] drbd p11 lima-dv-3: Handshake to peer 2 successful: Agreed network protocol version 125
[Tue Nov 17 19:52:10 2026] drbd p11 lima-dv-3: Feature flags enabled on protocol level: 0x7ff TRIM THIN_RESYNC WRITE_SAME WRITE_ZEROES RESYNC_DAGTAG WRITE_POSTPONE
[Tue Nov 17 19:52:10 2026] drbd p11: Preparing cluster-wide state change 940763327: 0->2 role( Primary ) conn( Connected )
[Tue Nov 17 19:52:10 2026] drbd p11/0 drbd1011 lima-dv-3: drbd_sync_handshake:
[Tue Nov 17 19:52:10 2026] drbd p11/0 drbd1011 lima-dv-3: self 92A73866FF86CC7E:0000000000000000:0000000000000000:0000000000000000 bits:8451 flags:2120
[Tue Nov 17 19:52:10 2026] drbd p11/0 drbd1011 lima-dv-3: peer 0000000000000004:0000000000000000:0000000000000000:0000000000000000 bits:262079 flags:2024
[Tue Nov 17 19:52:10 2026] drbd p11/0 drbd1011 lima-dv-3: uuid_compare()=source-set-bitmap by rule=just-created-peer
[Tue Nov 17 19:52:10 2026] drbd p11/0 drbd1011 lima-dv-3: Setting and writing one bitmap slot, after drbd_sync_handshake
[Tue Nov 17 19:52:10 2026] drbd p11: State change 940763327: primary_nodes=1, weak_nodes=FFFFFFFFFFFFFFF8
[Tue Nov 17 19:52:10 2026] drbd p11: Committing cluster-wide state change 940763327 (12ms)
[Tue Nov 17 19:52:10 2026] drbd p11 lima-dv-3: conn( Connecting -> Connected ) peer( Unknown -> Secondary ) [connected]
[Tue Nov 17 19:52:10 2026] drbd p11/0 drbd1011 lima-dv-3: pdsk( DUnknown -> Inconsistent ) repl( Off -> WFBitMapS ) replication( yes -> no ) [connected]
[Tue Nov 17 19:52:10 2026] drbd p11: Preparing remote state change 1889604076: 1->2 role( Secondary ) conn( Connected )
[Tue Nov 17 19:52:10 2026] drbd p11/0 drbd1011 lima-dv-3: send bitmap stats [Bytes(packets)]: plain 0(0), RLE 21(1), total 21; compression: 100.0%
[Tue Nov 17 19:52:10 2026] drbd p11/0 drbd1011 lima-dv-3: receive bitmap stats [Bytes(packets)]: plain 0(0), RLE 21(1), total 21; compression: 100.0%
[Tue Nov 17 19:52:10 2026] drbd p11/0 drbd1011 lima-dv-3: repl( WFBitMapS -> SyncSource ) [receive-bitmap]
[Tue Nov 17 19:52:10 2026] drbd p11/0 drbd1011 lima-dv-3: Began resync as SyncSource (will sync 1048316 KB [262079 bits set]).
[Tue Nov 17 19:52:10 2026] drbd p11 lima-dv-2: Aborting remote state change 1889604076
[Tue Nov 17 19:52:10 2026] drbd p11: Preparing remote state change 2456370903: 1->2 role( Secondary ) conn( Connected )
[Tue Nov 17 19:52:10 2026] drbd p11 lima-dv-2: Committing remote state change 2456370903 (primary_nodes=1)
[Tue Nov 17 19:53:18 2026] drbd p11/0 drbd1011 lima-dv-3: replication( no -> yes ) [enable-replication]
[Tue Nov 17 19:53:22 2026] drbd p11/0 drbd1011 lima-dv-3: Resync done (total 71 sec; paused 0 sec; 15216 K/sec)
[Tue Nov 17 19:53:22 2026] drbd p11/0 drbd1011 lima-dv-3: pdsk( Inconsistent -> UpToDate ) repl( SyncSource -> Established ) [resync-finished]
[Tue Nov 17 19:53:34 2026] drbd p11: Preparing cluster-wide state change 2857370051: 0->all role( Secondary )
[Tue Nov 17 19:53:34 2026] drbd p11: State change 2857370051: primary_nodes=0, weak_nodes=0
[Tue Nov 17 19:53:34 2026] drbd p11: Committing cluster-wide state change 2857370051 (0ms)
[Tue Nov 17 19:53:34 2026] drbd p11: role( Primary -> Secondary ) [umount:261175 auto-demote]
--- dmesg -T | grep 'drbd p11' | tail -n 80 [dv-2]
[Tue Nov 17 19:51:29 2026] drbd p11 lima-dv-1: conn( Connecting -> Connected ) peer( Unknown -> Secondary ) [remote]
[Tue Nov 17 19:51:29 2026] drbd p11/0 drbd1011: quorum( no -> yes ) [remote]
[Tue Nov 17 19:51:29 2026] drbd p11/0 drbd1011 lima-dv-1: pdsk( DUnknown -> Inconsistent ) repl( Off -> Established ) replication( yes -> no ) peer_replication( yes -> no ) [remote]
[Tue Nov 17 19:51:29 2026] drbd p11/0 drbd1011: Resumed AL updates
[Tue Nov 17 19:51:30 2026] drbd p11 lima-dv-3: Handshake to peer 2 successful: Agreed network protocol version 125
[Tue Nov 17 19:51:30 2026] drbd p11 lima-dv-3: Feature flags enabled on protocol level: 0x7ff TRIM THIN_RESYNC WRITE_SAME WRITE_ZEROES RESYNC_DAGTAG WRITE_POSTPONE
[Tue Nov 17 19:51:30 2026] drbd p11: Preparing remote state change 763770148: 0->2 role( Secondary ) conn( Connected )
[Tue Nov 17 19:51:30 2026] drbd p11 lima-dv-1: Committing remote state change 763770148 (primary_nodes=0)
[Tue Nov 17 19:51:30 2026] drbd p11: Preparing cluster-wide state change 3926141265: 1->2 role( Secondary ) conn( Connected )
[Tue Nov 17 19:51:30 2026] drbd p11/0 drbd1011 lima-dv-3: self 0000000000000004:0000000000000000:0000000000000000:0000000000000000 bits:0 flags:0
[Tue Nov 17 19:51:30 2026] drbd p11/0 drbd1011 lima-dv-3: peer's exposed UUID: 0000000000000000
[Tue Nov 17 19:51:30 2026] drbd p11: State change 3926141265: primary_nodes=0, weak_nodes=0
[Tue Nov 17 19:51:30 2026] drbd p11: Committing cluster-wide state change 3926141265 (16ms)
[Tue Nov 17 19:51:30 2026] drbd p11 lima-dv-3: conn( Connecting -> Connected ) peer( Unknown -> Secondary ) [connected]
[Tue Nov 17 19:51:30 2026] drbd p11/0 drbd1011 lima-dv-3: pdsk( DUnknown -> Diskless ) repl( Off -> Established ) peer_replication( yes -> no ) [connected]
[Tue Nov 17 19:51:36 2026] drbd p11/0 drbd1011: Accepted new current UUID, preparing to skip initial sync
[Tue Nov 17 19:51:36 2026] drbd p11/0 drbd1011: disk( Inconsistent -> UpToDate ) [skip-initial-sync]
[Tue Nov 17 19:51:36 2026] drbd p11/0 drbd1011 lima-dv-1: pdsk( Inconsistent -> UpToDate ) replication( no -> yes ) [skip-initial-sync]
[Tue Nov 17 19:51:36 2026] drbd p11/0 drbd1011: persisting effective size = 1024 MB (1048316 KB)
[Tue Nov 17 19:51:36 2026] drbd p11/0 drbd1011 lima-dv-1: receiver updated UUIDs to 92A73866FF86CC7E:0000000000000000:0000000000000000:0000000000000000
[Tue Nov 17 19:51:41 2026] drbd p11: Preparing remote state change 728516217: 0->all role( Primary )
[Tue Nov 17 19:51:41 2026] drbd p11 lima-dv-1: Committing remote state change 728516217 (primary_nodes=1)
[Tue Nov 17 19:51:41 2026] drbd p11 lima-dv-1: peer( Secondary -> Primary ) [remote]
[Tue Nov 17 19:51:41 2026] drbd p11: Preparing remote state change 1523522869: 0->all role( Secondary )
[Tue Nov 17 19:51:41 2026] drbd p11 lima-dv-1: Committing remote state change 1523522869 (primary_nodes=0)
[Tue Nov 17 19:51:41 2026] drbd p11 lima-dv-1: peer( Primary -> Secondary ) [remote]
[Tue Nov 17 19:51:41 2026] drbd p11: Preparing remote state change 597684032: 0->all role( Primary )
[Tue Nov 17 19:51:41 2026] drbd p11 lima-dv-1: Committing remote state change 597684032 (primary_nodes=1)
[Tue Nov 17 19:51:41 2026] drbd p11 lima-dv-1: peer( Secondary -> Primary ) [remote]
[Tue Nov 17 19:51:52 2026] drbd p11/0 drbd1011 lima-dv-3: Former intentional diskless peer got bitmap slot 1
[Tue Nov 17 19:51:58 2026] drbd p11/0 drbd1011: md_sync_timer expired! Worker calls drbd_md_sync().
[Tue Nov 17 19:52:02 2026] drbd p11: Preparing remote state change 1491530933: 2->0 conn( Disconnecting ) disk( Outdated )
[Tue Nov 17 19:52:02 2026] drbd p11 lima-dv-3: Committing remote state change 1491530933 (primary_nodes=1)
[Tue Nov 17 19:52:02 2026] drbd p11: Preparing remote state change 1079172654: 2->1 conn( Disconnecting )
[Tue Nov 17 19:52:02 2026] drbd p11 lima-dv-3: Committing remote state change 1079172654 (primary_nodes=1)
[Tue Nov 17 19:52:02 2026] drbd p11 lima-dv-3: conn( Connected -> TearDown ) peer( Secondary -> Unknown ) [remote]
[Tue Nov 17 19:52:02 2026] drbd p11/0 drbd1011 lima-dv-3: pdsk( Diskless -> DUnknown ) repl( Established -> Off ) [remote]
[Tue Nov 17 19:52:02 2026] drbd p11 lima-dv-3: Terminating sender thread
[Tue Nov 17 19:52:02 2026] drbd p11 lima-dv-3: Starting sender thread (peer-node-id 2)
[Tue Nov 17 19:52:02 2026] drbd p11 lima-dv-3: Connection closed
[Tue Nov 17 19:52:02 2026] drbd p11 lima-dv-3: conn( TearDown -> Unconnected ) [disconnected]
[Tue Nov 17 19:52:02 2026] drbd p11 lima-dv-3: Restarting receiver thread
[Tue Nov 17 19:52:02 2026] drbd p11 lima-dv-3: conn( Unconnected -> Connecting ) [connecting]
[Tue Nov 17 19:52:11 2026] drbd p11 lima-dv-3: Handshake to peer 2 successful: Agreed network protocol version 125
[Tue Nov 17 19:52:11 2026] drbd p11 lima-dv-3: Feature flags enabled on protocol level: 0x7ff TRIM THIN_RESYNC WRITE_SAME WRITE_ZEROES RESYNC_DAGTAG WRITE_POSTPONE
[Tue Nov 17 19:52:11 2026] drbd p11: Preparing remote state change 940763327: 0->2 role( Primary ) conn( Connected )
[Tue Nov 17 19:52:11 2026] drbd p11 lima-dv-1: Committing remote state change 940763327 (primary_nodes=1)
[Tue Nov 17 19:52:11 2026] drbd p11: Preparing cluster-wide state change 1889604076: 1->2 role( Secondary ) conn( Connected )
[Tue Nov 17 19:52:11 2026] drbd p11/0 drbd1011 lima-dv-3: drbd_sync_handshake:
[Tue Nov 17 19:52:11 2026] drbd p11/0 drbd1011 lima-dv-3: self 92A73866FF86CC7E:0000000000000000:0000000000000000:0000000000000000 bits:8451 flags:2100
[Tue Nov 17 19:52:11 2026] drbd p11/0 drbd1011 lima-dv-3: peer 0000000000000004:0000000000000000:0000000000000000:0000000000000000 bits:262079 flags:4
[Tue Nov 17 19:52:11 2026] drbd p11/0 drbd1011 lima-dv-3: uuid_compare()=source-set-bitmap by rule=just-created-peer
[Tue Nov 17 19:52:11 2026] drbd p11/0 drbd1011 lima-dv-3: Setting and writing one bitmap slot, after drbd_sync_handshake
[Tue Nov 17 19:52:11 2026] drbd p11: Aborting cluster-wide state change 1889604076 (16ms) rv = -19
[Tue Nov 17 19:52:11 2026] drbd p11: Preparing cluster-wide state change 2456370903: 1->2 role( Secondary ) conn( Connected )
[Tue Nov 17 19:52:11 2026] drbd p11/0 drbd1011 lima-dv-3: drbd_sync_handshake:
[Tue Nov 17 19:52:11 2026] drbd p11/0 drbd1011 lima-dv-3: self 92A73866FF86CC7E:0000000000000000:0000000000000000:0000000000000000 bits:262079 flags:2100
[Tue Nov 17 19:52:11 2026] drbd p11/0 drbd1011 lima-dv-3: peer 92A73866FF86CC7E:168BA36802061718:0000000000000000:0000000000000000 bits:262079 flags:1804
[Tue Nov 17 19:52:11 2026] drbd p11/0 drbd1011 lima-dv-3: uuid_compare()=no-sync by rule=lost-quorum
[Tue Nov 17 19:52:11 2026] drbd p11/0 drbd1011 lima-dv-3: strategy = source-set-bitmap due to disk states. (UpToDate/Inconsistent)
[Tue Nov 17 19:52:11 2026] drbd p11/0 drbd1011 lima-dv-3: Setting and writing one bitmap slot, after drbd_sync_handshake
[Tue Nov 17 19:52:11 2026] drbd p11/0 drbd1011 lima-dv-3: updated bitmap UUID 92A73866FF86CC7E:92A73866FF86CC7E:0000000000000000:0000000000000000
[Tue Nov 17 19:52:11 2026] drbd p11: State change 2456370903: primary_nodes=1, weak_nodes=FFFFFFFFFFFFFFF8
[Tue Nov 17 19:52:11 2026] drbd p11: Committing cluster-wide state change 2456370903 (16ms)
[Tue Nov 17 19:52:11 2026] drbd p11 lima-dv-3: conn( Connecting -> Connected ) peer( Unknown -> Secondary ) [connected]
[Tue Nov 17 19:52:11 2026] drbd p11/0 drbd1011 lima-dv-3: pdsk( DUnknown -> Inconsistent ) repl( Off -> WFBitMapS ) resync-susp( no -> peer ) replication( yes -> no ) [connected]
[Tue Nov 17 19:52:11 2026] drbd p11/0 drbd1011 lima-dv-3: send bitmap stats [Bytes(packets)]: plain 0(0), RLE 21(1), total 21; compression: 100.0%
[Tue Nov 17 19:52:11 2026] drbd p11/0 drbd1011 lima-dv-3: receive bitmap stats [Bytes(packets)]: plain 0(0), RLE 21(1), total 21; compression: 100.0%
[Tue Nov 17 19:52:11 2026] drbd p11/0 drbd1011 lima-dv-3: repl( WFBitMapS -> PausedSyncS ) [receive-bitmap]
[Tue Nov 17 19:52:11 2026] drbd p11/0 drbd1011 lima-dv-3: Began resync as PausedSyncS (will sync 1048316 KB [262079 bits set]).
[Tue Nov 17 19:53:18 2026] drbd p11/0 drbd1011 lima-dv-3: replication( no -> yes ) [enable-replication]
[Tue Nov 17 19:53:23 2026] drbd p11/0 drbd1011 lima-dv-3: updated UUIDs 92A73866FF86CC7E:0000000000000000:0000000000000000:0000000000000000
[Tue Nov 17 19:53:23 2026] drbd p11/0 drbd1011 lima-dv-3: Resync done (total 71 sec; paused 0 sec; 15104 K/sec)
[Tue Nov 17 19:53:23 2026] drbd p11/0 drbd1011 lima-dv-3: expected n_oos:505 to be 0
[Tue Nov 17 19:53:23 2026] drbd p11/0 drbd1011 lima-dv-3: pdsk( Inconsistent -> UpToDate ) repl( PausedSyncS -> Established ) [resync-finished]
[Tue Nov 17 19:53:23 2026] drbd p11/0 drbd1011 lima-dv-3: resync-susp( peer -> no ) [peer-state]
[Tue Nov 17 19:53:29 2026] drbd p11/0 drbd1011: md_sync_timer expired! Worker calls drbd_md_sync().
[Tue Nov 17 19:53:35 2026] drbd p11: Preparing remote state change 2857370051: 0->all role( Secondary )
[Tue Nov 17 19:53:35 2026] drbd p11 lima-dv-1: Committing remote state change 2857370051 (primary_nodes=0)
[Tue Nov 17 19:53:35 2026] drbd p11 lima-dv-1: peer( Primary -> Secondary ) [remote]
--- dmesg -T | grep 'drbd p11' | tail -n 80 [dv-3]
[Tue Nov 17 19:52:01 2026] drbd p11 lima-dv-2: Terminating receiver thread
[Tue Nov 17 19:52:01 2026] drbd p11 lima-dv-2: Terminating sender thread
[Tue Nov 17 19:52:10 2026] drbd p11: Starting worker thread (node-id 2)
[Tue Nov 17 19:52:10 2026] drbd p11 lima-dv-1: Starting sender thread (peer-node-id 0)
[Tue Nov 17 19:52:10 2026] drbd p11 lima-dv-2: Starting sender thread (peer-node-id 1)
[Tue Nov 17 19:52:10 2026] drbd p11/0 drbd1011: meta-data IO uses: blk-bio
[Tue Nov 17 19:52:10 2026] drbd p11/0 drbd1011: disk( Diskless -> Attaching ) [attach]
[Tue Nov 17 19:52:10 2026] drbd p11/0 drbd1011: Maximum number of peer devices = 7
[Tue Nov 17 19:52:10 2026] drbd p11: Method to ensure write ordering: flush
[Tue Nov 17 19:52:10 2026] drbd p11/0 drbd1011: Allocating 224 KiB for new bitmap
[Tue Nov 17 19:52:10 2026] drbd p11/0 drbd1011: resync bitmap: bits=262079 bits_4k=262079 words=28665 pages=56
[Tue Nov 17 19:52:10 2026] drbd p11/0 drbd1011: size = 1024 MB (1048316 KB)
[Tue Nov 17 19:52:10 2026] drbd p11/0 drbd1011: Suspended AL updates
[Tue Nov 17 19:52:10 2026] drbd p11/0 drbd1011: disk( Attaching -> Inconsistent ) [attach]
[Tue Nov 17 19:52:10 2026] drbd p11/0 drbd1011: attached to current UUID: 0000000000000004
[Tue Nov 17 19:52:10 2026] drbd p11 lima-dv-1: conn( StandAlone -> Unconnected ) [connect]
[Tue Nov 17 19:52:10 2026] drbd p11 lima-dv-2: conn( StandAlone -> Unconnected ) [connect]
[Tue Nov 17 19:52:10 2026] drbd p11 lima-dv-1: Starting receiver thread (peer-node-id 0)
[Tue Nov 17 19:52:10 2026] drbd p11 lima-dv-2: Starting receiver thread (peer-node-id 1)
[Tue Nov 17 19:52:10 2026] drbd p11 lima-dv-1: conn( Unconnected -> Connecting ) [connecting]
[Tue Nov 17 19:52:10 2026] drbd p11 lima-dv-2: conn( Unconnected -> Connecting ) [connecting]
[Tue Nov 17 19:52:11 2026] drbd p11 lima-dv-1: Handshake to peer 0 successful: Agreed network protocol version 125
[Tue Nov 17 19:52:11 2026] drbd p11 lima-dv-1: Feature flags enabled on protocol level: 0x7ff TRIM THIN_RESYNC WRITE_SAME WRITE_ZEROES RESYNC_DAGTAG WRITE_POSTPONE
[Tue Nov 17 19:52:11 2026] drbd p11 lima-dv-2: Handshake to peer 1 successful: Agreed network protocol version 125
[Tue Nov 17 19:52:11 2026] drbd p11 lima-dv-2: Feature flags enabled on protocol level: 0x7ff TRIM THIN_RESYNC WRITE_SAME WRITE_ZEROES RESYNC_DAGTAG WRITE_POSTPONE
[Tue Nov 17 19:52:11 2026] drbd p11: Preparing remote state change 940763327: 0->2 role( Primary ) conn( Connected )
[Tue Nov 17 19:52:11 2026] drbd p11/0 drbd1011 lima-dv-1: drbd_sync_handshake:
[Tue Nov 17 19:52:11 2026] drbd p11/0 drbd1011 lima-dv-1: self 0000000000000004:0000000000000000:0000000000000000:0000000000000000 bits:262079 flags:2024
[Tue Nov 17 19:52:11 2026] drbd p11/0 drbd1011 lima-dv-1: peer 92A73866FF86CC7E:0000000000000000:0000000000000000:0000000000000000 bits:8451 flags:3120
[Tue Nov 17 19:52:11 2026] drbd p11/0 drbd1011 lima-dv-1: uuid_compare()=target-set-bitmap by rule=just-created-self
[Tue Nov 17 19:52:11 2026] drbd p11/0 drbd1011 lima-dv-1: Setting and writing the whole bitmap, fresh node
[Tue Nov 17 19:52:11 2026] drbd p11 lima-dv-1: Committing remote state change 940763327 (primary_nodes=1)
[Tue Nov 17 19:52:11 2026] drbd p11 lima-dv-1: conn( Connecting -> Connected ) peer( Unknown -> Primary ) [remote]
[Tue Nov 17 19:52:11 2026] drbd p11/0 drbd1011: quorum( no -> yes ) [remote]
[Tue Nov 17 19:52:11 2026] drbd p11/0 drbd1011 lima-dv-1: pdsk( DUnknown -> UpToDate ) repl( Off -> WFBitMapT ) peer_replication( yes -> no ) [remote]
[Tue Nov 17 19:52:11 2026] drbd p11/0 drbd1011: Resumed AL updates
[Tue Nov 17 19:52:11 2026] drbd p11/0 drbd1011: Setting exposed data uuid: 92A73866FF86CC7E
[Tue Nov 17 19:52:11 2026] drbd p11/0 drbd1011: persisting effective size = 1024 MB (1048316 KB)
[Tue Nov 17 19:52:11 2026] drbd p11: Preparing remote state change 1889604076: 1->2 role( Secondary ) conn( Connected )
[Tue Nov 17 19:52:11 2026] drbd p11/0 drbd1011 lima-dv-1: receive bitmap stats [Bytes(packets)]: plain 0(0), RLE 21(1), total 21; compression: 100.0%
[Tue Nov 17 19:52:11 2026] drbd p11/0 drbd1011 lima-dv-1: send bitmap stats [Bytes(packets)]: plain 0(0), RLE 21(1), total 21; compression: 100.0%
[Tue Nov 17 19:52:11 2026] drbd p11/0 drbd1011 lima-dv-2: resync-susp( no -> connection dependency ) [receive-bitmap]
[Tue Nov 17 19:52:11 2026] drbd p11/0 drbd1011 lima-dv-1: repl( WFBitMapT -> SyncTarget ) [receive-bitmap]
[Tue Nov 17 19:52:11 2026] drbd p11/0 drbd1011 lima-dv-1: Began resync as SyncTarget (will sync 1048316 KB [262079 bits set]).
[Tue Nov 17 19:52:11 2026] drbd p11/0 drbd1011: Bitmap UUID for initial resync: 168BA36802061718
[Tue Nov 17 19:52:11 2026] drbd p11/0 drbd1011 lima-dv-2: drbd_sync_handshake:
[Tue Nov 17 19:52:11 2026] drbd p11/0 drbd1011 lima-dv-2: self 92A73866FF86CC7E:168BA36802061718:0000000000000000:0000000000000000 bits:262079 flags:4
[Tue Nov 17 19:52:11 2026] drbd p11/0 drbd1011 lima-dv-2: peer 92A73866FF86CC7E:0000000000000000:0000000000000000:0000000000000000 bits:8451 flags:2100
[Tue Nov 17 19:52:11 2026] drbd p11/0 drbd1011 lima-dv-2: My current UUID changed during handshake.
[Tue Nov 17 19:52:11 2026] drbd p11/0 drbd1011 lima-dv-2: My bitmap UUID changed during handshake. 0x0 to 0x168BA36802061718
[Tue Nov 17 19:52:11 2026] drbd p11/0 drbd1011 lima-dv-2: My uuid_flags changed from 0x4 to 0x804 during handshake.
[Tue Nov 17 19:52:11 2026] drbd p11/0 drbd1011 lima-dv-2: uuid_compare()=retry-connect by rule=initial-handshake-changed
[Tue Nov 17 19:52:11 2026] drbd p11 lima-dv-2: Aborting remote state change 1889604076
[Tue Nov 17 19:52:11 2026] drbd p11: Preparing remote state change 2456370903: 1->2 role( Secondary ) conn( Connected )
[Tue Nov 17 19:52:11 2026] drbd p11/0 drbd1011 lima-dv-2: drbd_sync_handshake:
[Tue Nov 17 19:52:11 2026] drbd p11/0 drbd1011 lima-dv-2: self 92A73866FF86CC7E:168BA36802061718:0000000000000000:0000000000000000 bits:262079 flags:4
[Tue Nov 17 19:52:11 2026] drbd p11/0 drbd1011 lima-dv-2: peer 92A73866FF86CC7E:0000000000000000:0000000000000000:0000000000000000 bits:262079 flags:2100
[Tue Nov 17 19:52:11 2026] drbd p11/0 drbd1011 lima-dv-2: uuid_compare()=no-sync by rule=lost-quorum
[Tue Nov 17 19:52:11 2026] drbd p11/0 drbd1011 lima-dv-2: strategy = target-set-bitmap due to disk states. (Inconsistent/UpToDate)
[Tue Nov 17 19:52:11 2026] drbd p11 lima-dv-2: Committing remote state change 2456370903 (primary_nodes=1)
[Tue Nov 17 19:52:11 2026] drbd p11 lima-dv-2: conn( Connecting -> Connected ) peer( Unknown -> Secondary ) [remote]
[Tue Nov 17 19:52:11 2026] drbd p11/0 drbd1011 lima-dv-2: pdsk( DUnknown -> UpToDate ) repl( Off -> WFBitMapT ) peer_replication( yes -> no ) [remote]
[Tue Nov 17 19:52:11 2026] drbd p11/0 drbd1011 lima-dv-2: receive bitmap stats [Bytes(packets)]: plain 0(0), RLE 21(1), total 21; compression: 100.0%
[Tue Nov 17 19:52:11 2026] drbd p11/0 drbd1011 lima-dv-2: send bitmap stats [Bytes(packets)]: plain 0(0), RLE 21(1), total 21; compression: 100.0%
[Tue Nov 17 19:52:11 2026] drbd p11/0 drbd1011 lima-dv-2: repl( WFBitMapT -> PausedSyncT ) [receive-bitmap]
[Tue Nov 17 19:52:11 2026] drbd p11/0 drbd1011 lima-dv-2: Began resync as PausedSyncT (will sync 1048316 KB [262079 bits set]).
[Tue Nov 17 19:52:11 2026] drbd p11/0 drbd1011: Bitmap UUID for initial resync: 13FB475862C07C54
[Tue Nov 17 19:52:17 2026] drbd p11/0 drbd1011: md_sync_timer expired! Worker calls drbd_md_sync().
[Tue Nov 17 19:53:18 2026] drbd p11/0 drbd1011 lima-dv-2: peer_replication( no -> yes ) [enable-peer-replication]
[Tue Nov 17 19:53:18 2026] drbd p11/0 drbd1011 lima-dv-1: peer_replication( no -> yes ) [enable-peer-replication]
[Tue Nov 17 19:53:22 2026] drbd p11/0 drbd1011 lima-dv-1: Resync done (total 71 sec; paused 0 sec; 15216 K/sec)
[Tue Nov 17 19:53:22 2026] drbd p11/0 drbd1011 lima-dv-1: updated UUIDs 92A73866FF86CC7E:0000000000000000:E0B8AA77D93AE54C:0000000000000000
[Tue Nov 17 19:53:22 2026] drbd p11/0 drbd1011: disk( Inconsistent -> UpToDate ) [resync-finished]
[Tue Nov 17 19:53:22 2026] drbd p11/0 drbd1011 lima-dv-2: repl( PausedSyncT -> Established ) resync-susp( connection dependency -> no ) [resync-finished]
[Tue Nov 17 19:53:22 2026] drbd p11/0 drbd1011 lima-dv-1: repl( SyncTarget -> Established ) [resync-finished]
[Tue Nov 17 19:53:22 2026] drbd p11/0 drbd1011 lima-dv-1: Sync source can not get this write, postponing it towards the writer
[Tue Nov 17 19:53:28 2026] drbd p11/0 drbd1011: md_sync_timer expired! Worker calls drbd_md_sync().
[Tue Nov 17 19:53:34 2026] drbd p11: Preparing remote state change 2857370051: 0->all role( Secondary )
[Tue Nov 17 19:53:34 2026] drbd p11 lima-dv-1: Committing remote state change 2857370051 (primary_nodes=0)
[Tue Nov 17 19:53:34 2026] drbd p11 lima-dv-1: peer( Primary -> Secondary ) [remote]
P11 run 2 result: dv-3 UpToDate=1
exit=0
```

### P12. Уход стороны с копией и стороны без диска

Ресурс `p12`: `dv-1`, `dv-2`, `dv-3` с копией (1 ГиБ), port 7012, minor 1012; в шаге 6 — `dv-4` без диска.

```bash
stand/spike/pre-2b.sh p12
```

```text
### P12 steps 1-5: three copies, dv-3 leaves
$ [dv-1] drbdadm create-md --force --max-peers=7 p12
initializing bitmap (224 KB) to all zero
initializing activity log
Writing meta data...
New drbd meta data block successfully created.
rc=0
$ [dv-2] drbdadm create-md --force --max-peers=7 p12
initializing bitmap (224 KB) to all zero
initializing activity log
Writing meta data...
New drbd meta data block successfully created.
rc=0
$ [dv-3] drbdadm create-md --force --max-peers=7 p12
initializing bitmap (224 KB) to all zero
initializing activity log
Writing meta data...
New drbd meta data block successfully created.
rc=0
$ [dv-1] drbdadm up p12
rc=0
$ [dv-2] drbdadm up p12
rc=0
$ [dv-3] drbdadm up p12
rc=0
wait: dv-1: 2 connections Connected after 2 s at most
wait: dv-2: 2 connections Connected after 2 s at most
wait: dv-3: 2 connections Connected after 2 s at most
$ [dv-1] drbdadm new-current-uuid --clear-bitmap p12/0
rc=0
$ [dv-1] drbdsetup status p12 --verbose
p12 node-id:0 role:Secondary suspended:no force-io-failures:no
  volume:0 minor:1012 disk:UpToDate backing_dev:/dev/ambar-exp/p12 quorum:yes
      open:no blocked:no
  lima-dv-2 node-id:1 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
  lima-dv-3 node-id:2 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no

rc=0
$ [dv-1] mkfs.ext4 -q -F -E nodiscard /dev/drbd1012
rc=0
19:55:22.191
1566
started
@ 19:55:24.011 down on dv-3
$ [dv-3] drbdadm down p12
rc=0
$ [dv-1] drbdsetup status p12 --verbose
p12 node-id:0 role:Primary suspended:no force-io-failures:no
  volume:0 minor:1012 disk:UpToDate backing_dev:/dev/ambar-exp/p12 quorum:yes
      open:yes blocked:no
  lima-dv-2 node-id:1 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:16 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
  lima-dv-3 node-id:2 connection:Connecting role:Unknown tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Off peer-disk:Outdated resync-suspended:no

rc=0
$ [dv-2] drbdsetup status p12 --verbose
p12 node-id:1 role:Secondary suspended:no force-io-failures:no
  volume:0 minor:1012 disk:UpToDate backing_dev:/dev/ambar-exp/p12 quorum:yes
      open:no blocked:no
  lima-dv-1 node-id:0 connection:Connected role:Primary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
  lima-dv-3 node-id:2 connection:Connecting role:Unknown tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Off peer-disk:Outdated resync-suspended:no

rc=0
@ 19:56:01.257 30 s after down; dv-1 quorum now
$ [dv-1] drbdsetup status p12 --verbose | grep -o 'quorum:[a-z]*'
quorum:yes
rc=0

### P12 step 2: p12.res without dv-3 on dv-1 and dv-2, adjust
@ 19:56:04.336 adjust on dv-1
$ [dv-1] drbdadm adjust p12
rc=0
$ [dv-2] drbdadm adjust p12
rc=0
$ [dv-1] drbdsetup status p12 --verbose --statistics
p12 node-id:0 role:Primary suspended:no force-io-failures:no
    write-ordering:flush
  volume:0 minor:1012 disk:UpToDate backing_dev:/dev/ambar-exp/p12 quorum:yes
      open:yes
      size:1048316 read:1005 written:377644 al-writes:11 bm-writes:69 upper-pending:0
      lower-pending:0 al-suspended:no blocked:no
  lima-dv-2 node-id:1 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:8 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
        received:0 sent:344748 out-of-sync:0 pending:0 unacked:0

rc=0
$ [dv-2] drbdsetup status p12 --verbose --statistics
p12 node-id:1 role:Secondary suspended:no force-io-failures:no
    write-ordering:flush
  volume:0 minor:1012 disk:UpToDate backing_dev:/dev/ambar-exp/p12 quorum:yes
      open:no
      size:1048316 read:0 written:380360 al-writes:11 bm-writes:69 upper-pending:0
      lower-pending:0 al-suspended:no blocked:no
  lima-dv-1 node-id:0 connection:Connected role:Primary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
        received:347464 sent:0 out-of-sync:0 pending:0 unacked:0

rc=0

### P12 step 3: forget-peer for node-id 2 on dv-1, twice
@ 19:56:15.253 forget-peer 1
$ [dv-1] drbdsetup forget-peer p12 2
rc=0
@ 19:56:16.914 forget-peer 2
$ [dv-1] drbdsetup forget-peer p12 2
rc=0
$ [dv-1] drbdsetup status p12 --verbose
p12 node-id:0 role:Primary suspended:no force-io-failures:no
  volume:0 minor:1012 disk:UpToDate backing_dev:/dev/ambar-exp/p12 quorum:yes
      open:yes blocked:no
  lima-dv-2 node-id:1 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:8 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no

rc=0

### P12 step 4: forget-peer for node-id 5 (not in the metadata) on dv-2
$ [dv-2] drbdsetup forget-peer p12 5
rc=0

### P12 step 5: stop the writer
@ 19:56:26.048 stop the writer
acked=23484 on-volume=23485
--- probe.sh log (stdout and stderr)
--- watch.sh log
samples=135 probe=alive:R=8 probe=alive:S=127 longest-gap-without-new-acked=0.5s (from 19:55:23.853)
--- quorum log of dv-1 (runs of equal values: first - last  value  samples)
19:55:23.885 - 19:56:27.254  "quorum": true  x64
--- dmesg -T | grep 'drbd p12' | tail -n 80 [dv-1]
[Tue Nov 17 19:55:00 2026] drbd p12 lima-dv-2: Feature flags enabled on protocol level: 0x7ff TRIM THIN_RESYNC WRITE_SAME WRITE_ZEROES RESYNC_DAGTAG WRITE_POSTPONE
[Tue Nov 17 19:55:00 2026] drbd p12: Preparing cluster-wide state change 2603897830: 0->1 role( Secondary ) conn( Connected )
[Tue Nov 17 19:55:00 2026] drbd p12/0 drbd1012 lima-dv-2: drbd_sync_handshake:
[Tue Nov 17 19:55:00 2026] drbd p12/0 drbd1012 lima-dv-2: self 0000000000000004:0000000000000000:0000000000000000:0000000000000000 bits:262079 flags:2024
[Tue Nov 17 19:55:00 2026] drbd p12/0 drbd1012 lima-dv-2: peer 0000000000000004:0000000000000000:0000000000000000:0000000000000000 bits:262079 flags:2024
[Tue Nov 17 19:55:00 2026] drbd p12/0 drbd1012 lima-dv-2: uuid_compare()=no-sync by rule=just-created-both
[Tue Nov 17 19:55:00 2026] drbd p12: State change 2603897830: primary_nodes=0, weak_nodes=0
[Tue Nov 17 19:55:00 2026] drbd p12: Committing cluster-wide state change 2603897830 (20ms)
[Tue Nov 17 19:55:00 2026] drbd p12 lima-dv-2: conn( Connecting -> Connected ) peer( Unknown -> Secondary ) [connected]
[Tue Nov 17 19:55:00 2026] drbd p12/0 drbd1012: quorum( no -> yes ) [connected]
[Tue Nov 17 19:55:00 2026] drbd p12/0 drbd1012 lima-dv-2: pdsk( DUnknown -> Inconsistent ) repl( Off -> Established ) replication( yes -> no ) peer_replication( yes -> no ) [connected]
[Tue Nov 17 19:55:00 2026] drbd p12/0 drbd1012: Resumed AL updates
[Tue Nov 17 19:55:02 2026] drbd p12: Preparing remote state change 2970928659: 1->2 role( Secondary ) conn( Connected )
[Tue Nov 17 19:55:02 2026] drbd p12 lima-dv-3: Handshake to peer 2 successful: Agreed network protocol version 125
[Tue Nov 17 19:55:02 2026] drbd p12 lima-dv-3: Feature flags enabled on protocol level: 0x7ff TRIM THIN_RESYNC WRITE_SAME WRITE_ZEROES RESYNC_DAGTAG WRITE_POSTPONE
[Tue Nov 17 19:55:02 2026] drbd p12 lima-dv-2: Committing remote state change 2970928659 (primary_nodes=0)
[Tue Nov 17 19:55:02 2026] drbd p12: Preparing cluster-wide state change 1835999972: 0->2 role( Secondary ) conn( Connected )
[Tue Nov 17 19:55:02 2026] drbd p12/0 drbd1012 lima-dv-3: drbd_sync_handshake:
[Tue Nov 17 19:55:02 2026] drbd p12/0 drbd1012 lima-dv-3: self 0000000000000004:0000000000000000:0000000000000000:0000000000000000 bits:262079 flags:24
[Tue Nov 17 19:55:02 2026] drbd p12/0 drbd1012 lima-dv-3: peer 0000000000000004:0000000000000000:0000000000000000:0000000000000000 bits:262079 flags:24
[Tue Nov 17 19:55:02 2026] drbd p12/0 drbd1012 lima-dv-3: uuid_compare()=no-sync by rule=just-created-both
[Tue Nov 17 19:55:02 2026] drbd p12: State change 1835999972: primary_nodes=0, weak_nodes=0
[Tue Nov 17 19:55:02 2026] drbd p12: Committing cluster-wide state change 1835999972 (16ms)
[Tue Nov 17 19:55:02 2026] drbd p12 lima-dv-3: conn( Connecting -> Connected ) peer( Unknown -> Secondary ) [connected]
[Tue Nov 17 19:55:02 2026] drbd p12/0 drbd1012 lima-dv-3: pdsk( DUnknown -> Inconsistent ) repl( Off -> Established ) replication( yes -> no ) peer_replication( yes -> no ) [connected]
[Tue Nov 17 19:55:09 2026] drbd p12/0 drbd1012 lima-dv-3: Preparing to skip initial sync
[Tue Nov 17 19:55:09 2026] drbd p12/0 drbd1012 lima-dv-2: Preparing to skip initial sync
[Tue Nov 17 19:55:09 2026] drbd p12/0 drbd1012: Bitmap UUID for initial resync: 2A8BA52EB3E1D96B
[Tue Nov 17 19:55:09 2026] drbd p12/0 drbd1012: new current UUID: 2EE98D3589280E1C weak: FFFFFFFFFFFFFFF9
[Tue Nov 17 19:55:09 2026] drbd p12/0 drbd1012 lima-dv-3: cleared bitmap UUID 2EE98D3589280E1C:0000000000000000:0000000000000000:0000000000000000
[Tue Nov 17 19:55:09 2026] drbd p12/0 drbd1012 lima-dv-2: cleared bitmap UUID 2EE98D3589280E1C:0000000000000000:0000000000000000:0000000000000000
[Tue Nov 17 19:55:09 2026] drbd p12/0 drbd1012: disk( Inconsistent -> UpToDate ) [new-c-uuid]
[Tue Nov 17 19:55:09 2026] drbd p12/0 drbd1012 lima-dv-3: pdsk( Inconsistent -> UpToDate ) replication( no -> yes ) [new-c-uuid]
[Tue Nov 17 19:55:09 2026] drbd p12/0 drbd1012 lima-dv-2: pdsk( Inconsistent -> UpToDate ) replication( no -> yes ) [new-c-uuid]
[Tue Nov 17 19:55:09 2026] drbd p12/0 drbd1012: persisting effective size = 1024 MB (1048316 KB)
[Tue Nov 17 19:55:14 2026] drbd p12: Preparing cluster-wide state change 2573219996: 0->all role( Primary )
[Tue Nov 17 19:55:14 2026] drbd p12: State change 2573219996: primary_nodes=1, weak_nodes=FFFFFFFFFFFFFFF8
[Tue Nov 17 19:55:14 2026] drbd p12: Committing cluster-wide state change 2573219996 (0ms)
[Tue Nov 17 19:55:14 2026] drbd p12: role( Secondary -> Primary ) [mkfs.ext4:268313 auto-promote]
[Tue Nov 17 19:55:14 2026] drbd p12/0 drbd1012: gen-obligation: NONE -> ARMED(promoted)
[Tue Nov 17 19:55:14 2026] drbd p12/0 drbd1012: gen-obligation: ARMED(promoted) -> MINTING(promoted)
[Tue Nov 17 19:55:14 2026] drbd p12/0 drbd1012: gen-obligation: MINTING(promoted) -> DISCHARGED(promoted)
[Tue Nov 17 19:55:14 2026] drbd p12: Preparing cluster-wide state change 2400962777: 0->all role( Secondary )
[Tue Nov 17 19:55:14 2026] drbd p12: State change 2400962777: primary_nodes=0, weak_nodes=0
[Tue Nov 17 19:55:14 2026] drbd p12: Committing cluster-wide state change 2400962777 (0ms)
[Tue Nov 17 19:55:14 2026] drbd p12: role( Primary -> Secondary ) [mkfs.ext4:268313 auto-demote]
[Tue Nov 17 19:55:14 2026] drbd p12: Preparing cluster-wide state change 3507366725: 0->all role( Primary )
[Tue Nov 17 19:55:14 2026] drbd p12: State change 3507366725: primary_nodes=1, weak_nodes=FFFFFFFFFFFFFFF8
[Tue Nov 17 19:55:14 2026] drbd p12: Committing cluster-wide state change 3507366725 (0ms)
[Tue Nov 17 19:55:14 2026] drbd p12: role( Secondary -> Primary ) [mount:268340 auto-promote]
[Tue Nov 17 19:55:14 2026] drbd p12/0 drbd1012: gen-obligation: DISCHARGED(promoted) -> ARMED(promoted)
[Tue Nov 17 19:55:14 2026] drbd p12/0 drbd1012: gen-obligation: ARMED(promoted) -> MINTING(promoted)
[Tue Nov 17 19:55:14 2026] drbd p12/0 drbd1012: gen-obligation: MINTING(promoted) -> DISCHARGED(promoted)
[Tue Nov 17 19:55:23 2026] drbd p12: Preparing remote state change 2351429808: 2->0 conn( Disconnecting ) disk( Outdated )
[Tue Nov 17 19:55:23 2026] drbd p12 lima-dv-3: Committing remote state change 2351429808 (primary_nodes=1)
[Tue Nov 17 19:55:23 2026] drbd p12 lima-dv-3: conn( Connected -> TearDown ) peer( Secondary -> Unknown ) [remote]
[Tue Nov 17 19:55:23 2026] drbd p12/0 drbd1012 lima-dv-3: pdsk( UpToDate -> Outdated ) repl( Established -> Off ) [remote]
[Tue Nov 17 19:55:23 2026] drbd p12/0 drbd1012: gen-obligation: DISCHARGED(promoted) -> ARMED(peer-data-lost)
[Tue Nov 17 19:55:23 2026] drbd p12 lima-dv-3: Terminating sender thread
[Tue Nov 17 19:55:23 2026] drbd p12 lima-dv-3: Starting sender thread (peer-node-id 2)
[Tue Nov 17 19:55:23 2026] drbd p12/0 drbd1012: gen-obligation: ARMED(peer-data-lost) -> MINTING(peer-data-lost)
[Tue Nov 17 19:55:23 2026] drbd p12/0 drbd1012: new current UUID: 4AF2630D459D876B weak: FFFFFFFFFFFFFFFC
[Tue Nov 17 19:55:23 2026] drbd p12/0 drbd1012: gen-obligation: MINTING(peer-data-lost) -> DISCHARGED(peer-data-lost)
[Tue Nov 17 19:55:23 2026] drbd p12 lima-dv-3: Connection closed
[Tue Nov 17 19:55:23 2026] drbd p12 lima-dv-3: conn( TearDown -> Unconnected ) [disconnected]
[Tue Nov 17 19:55:23 2026] drbd p12 lima-dv-3: Restarting receiver thread
[Tue Nov 17 19:55:23 2026] drbd p12 lima-dv-3: conn( Unconnected -> Connecting ) [connecting]
[Tue Nov 17 19:55:23 2026] drbd p12: Preparing remote state change 4205220973: 2->1 conn( Disconnecting )
[Tue Nov 17 19:55:23 2026] drbd p12 lima-dv-2: Committing remote state change 4205220973 (primary_nodes=1)
[Tue Nov 17 19:56:03 2026] drbd p12 lima-dv-3: conn( Connecting -> Disconnecting ) [del-peer]
[Tue Nov 17 19:56:03 2026] drbd p12 lima-dv-3: Terminating sender thread
[Tue Nov 17 19:56:03 2026] drbd p12 lima-dv-3: Starting sender thread (peer-node-id 2)
[Tue Nov 17 19:56:03 2026] drbd p12 lima-dv-3: Connection closed
[Tue Nov 17 19:56:03 2026] drbd p12 lima-dv-3: conn( Disconnecting -> StandAlone ) [disconnected]
[Tue Nov 17 19:56:03 2026] drbd p12 lima-dv-3: Terminating receiver thread
[Tue Nov 17 19:56:03 2026] drbd p12 lima-dv-3: Terminating sender thread
[Tue Nov 17 19:56:25 2026] drbd p12: Preparing cluster-wide state change 53499271: 0->all role( Secondary )
[Tue Nov 17 19:56:25 2026] drbd p12: State change 53499271: primary_nodes=0, weak_nodes=0
[Tue Nov 17 19:56:25 2026] drbd p12: Committing cluster-wide state change 53499271 (0ms)
[Tue Nov 17 19:56:25 2026] drbd p12: role( Primary -> Secondary ) [umount:295025 auto-demote]
--- dmesg -T | grep 'drbd p12' | tail -n 80 [dv-2]
[Tue Nov 17 19:54:59 2026] drbd p12/0 drbd1012: disk( Diskless -> Attaching ) [attach]
[Tue Nov 17 19:54:59 2026] drbd p12/0 drbd1012: Maximum number of peer devices = 7
[Tue Nov 17 19:54:59 2026] drbd p12: Method to ensure write ordering: flush
[Tue Nov 17 19:54:59 2026] drbd p12/0 drbd1012: Allocating 224 KiB for new bitmap
[Tue Nov 17 19:54:59 2026] drbd p12/0 drbd1012: resync bitmap: bits=262079 bits_4k=262079 words=28665 pages=56
[Tue Nov 17 19:54:59 2026] drbd p12/0 drbd1012: size = 1024 MB (1048316 KB)
[Tue Nov 17 19:54:59 2026] drbd p12/0 drbd1012: Suspended AL updates
[Tue Nov 17 19:54:59 2026] drbd p12/0 drbd1012: disk( Attaching -> Inconsistent ) [attach]
[Tue Nov 17 19:54:59 2026] drbd p12/0 drbd1012: attached to current UUID: 0000000000000004
[Tue Nov 17 19:54:59 2026] drbd p12 lima-dv-1: conn( StandAlone -> Unconnected ) [connect]
[Tue Nov 17 19:54:59 2026] drbd p12 lima-dv-3: conn( StandAlone -> Unconnected ) [connect]
[Tue Nov 17 19:54:59 2026] drbd p12 lima-dv-1: Starting receiver thread (peer-node-id 0)
[Tue Nov 17 19:54:59 2026] drbd p12 lima-dv-1: conn( Unconnected -> Connecting ) [connecting]
[Tue Nov 17 19:54:59 2026] drbd p12 lima-dv-3: Starting receiver thread (peer-node-id 2)
[Tue Nov 17 19:54:59 2026] drbd p12 lima-dv-3: conn( Unconnected -> Connecting ) [connecting]
[Tue Nov 17 19:55:00 2026] drbd p12 lima-dv-1: Handshake to peer 0 successful: Agreed network protocol version 125
[Tue Nov 17 19:55:00 2026] drbd p12 lima-dv-1: Feature flags enabled on protocol level: 0x7ff TRIM THIN_RESYNC WRITE_SAME WRITE_ZEROES RESYNC_DAGTAG WRITE_POSTPONE
[Tue Nov 17 19:55:00 2026] drbd p12: Preparing remote state change 2603897830: 0->1 role( Secondary ) conn( Connected )
[Tue Nov 17 19:55:00 2026] drbd p12/0 drbd1012 lima-dv-1: drbd_sync_handshake:
[Tue Nov 17 19:55:00 2026] drbd p12/0 drbd1012 lima-dv-1: self 0000000000000004:0000000000000000:0000000000000000:0000000000000000 bits:262079 flags:2024
[Tue Nov 17 19:55:00 2026] drbd p12/0 drbd1012 lima-dv-1: peer 0000000000000004:0000000000000000:0000000000000000:0000000000000000 bits:262079 flags:2024
[Tue Nov 17 19:55:00 2026] drbd p12/0 drbd1012 lima-dv-1: uuid_compare()=no-sync by rule=just-created-both
[Tue Nov 17 19:55:00 2026] drbd p12 lima-dv-1: Committing remote state change 2603897830 (primary_nodes=0)
[Tue Nov 17 19:55:00 2026] drbd p12 lima-dv-1: conn( Connecting -> Connected ) peer( Unknown -> Secondary ) [remote]
[Tue Nov 17 19:55:00 2026] drbd p12/0 drbd1012: quorum( no -> yes ) [remote]
[Tue Nov 17 19:55:00 2026] drbd p12/0 drbd1012 lima-dv-1: pdsk( DUnknown -> Inconsistent ) repl( Off -> Established ) replication( yes -> no ) peer_replication( yes -> no ) [remote]
[Tue Nov 17 19:55:00 2026] drbd p12/0 drbd1012: Resumed AL updates
[Tue Nov 17 19:55:02 2026] drbd p12 lima-dv-3: Handshake to peer 2 successful: Agreed network protocol version 125
[Tue Nov 17 19:55:02 2026] drbd p12 lima-dv-3: Feature flags enabled on protocol level: 0x7ff TRIM THIN_RESYNC WRITE_SAME WRITE_ZEROES RESYNC_DAGTAG WRITE_POSTPONE
[Tue Nov 17 19:55:02 2026] drbd p12: Preparing cluster-wide state change 2970928659: 1->2 role( Secondary ) conn( Connected )
[Tue Nov 17 19:55:02 2026] drbd p12/0 drbd1012 lima-dv-3: drbd_sync_handshake:
[Tue Nov 17 19:55:02 2026] drbd p12/0 drbd1012 lima-dv-3: self 0000000000000004:0000000000000000:0000000000000000:0000000000000000 bits:262079 flags:24
[Tue Nov 17 19:55:02 2026] drbd p12/0 drbd1012 lima-dv-3: peer 0000000000000004:0000000000000000:0000000000000000:0000000000000000 bits:262079 flags:2024
[Tue Nov 17 19:55:02 2026] drbd p12/0 drbd1012 lima-dv-3: uuid_compare()=no-sync by rule=just-created-both
[Tue Nov 17 19:55:02 2026] drbd p12: State change 2970928659: primary_nodes=0, weak_nodes=0
[Tue Nov 17 19:55:02 2026] drbd p12: Committing cluster-wide state change 2970928659 (20ms)
[Tue Nov 17 19:55:02 2026] drbd p12 lima-dv-3: conn( Connecting -> Connected ) peer( Unknown -> Secondary ) [connected]
[Tue Nov 17 19:55:02 2026] drbd p12/0 drbd1012 lima-dv-3: pdsk( DUnknown -> Inconsistent ) repl( Off -> Established ) replication( yes -> no ) peer_replication( yes -> no ) [connected]
[Tue Nov 17 19:55:02 2026] drbd p12: Preparing remote state change 1835999972: 0->2 role( Secondary ) conn( Connected )
[Tue Nov 17 19:55:02 2026] drbd p12 lima-dv-1: Committing remote state change 1835999972 (primary_nodes=0)
[Tue Nov 17 19:55:09 2026] drbd p12/0 drbd1012: Accepted new current UUID, preparing to skip initial sync
[Tue Nov 17 19:55:09 2026] drbd p12/0 drbd1012 lima-dv-3: pdsk( Inconsistent -> UpToDate ) replication( no -> yes ) [peer-state]
[Tue Nov 17 19:55:09 2026] drbd p12/0 drbd1012: disk( Inconsistent -> UpToDate ) [skip-initial-sync]
[Tue Nov 17 19:55:09 2026] drbd p12/0 drbd1012 lima-dv-1: pdsk( Inconsistent -> UpToDate ) replication( no -> yes ) [skip-initial-sync]
[Tue Nov 17 19:55:09 2026] drbd p12/0 drbd1012: persisting effective size = 1024 MB (1048316 KB)
[Tue Nov 17 19:55:09 2026] drbd p12/0 drbd1012 lima-dv-1: receiver updated UUIDs to 2EE98D3589280E1C:0000000000000000:0000000000000000:0000000000000000
[Tue Nov 17 19:55:14 2026] drbd p12: Preparing remote state change 2573219996: 0->all role( Primary )
[Tue Nov 17 19:55:14 2026] drbd p12 lima-dv-1: Committing remote state change 2573219996 (primary_nodes=1)
[Tue Nov 17 19:55:14 2026] drbd p12 lima-dv-1: peer( Secondary -> Primary ) [remote]
[Tue Nov 17 19:55:14 2026] drbd p12: Preparing remote state change 2400962777: 0->all role( Secondary )
[Tue Nov 17 19:55:14 2026] drbd p12 lima-dv-1: Committing remote state change 2400962777 (primary_nodes=0)
[Tue Nov 17 19:55:14 2026] drbd p12 lima-dv-1: peer( Primary -> Secondary ) [remote]
[Tue Nov 17 19:55:15 2026] drbd p12: Preparing remote state change 3507366725: 0->all role( Primary )
[Tue Nov 17 19:55:15 2026] drbd p12 lima-dv-1: Committing remote state change 3507366725 (primary_nodes=1)
[Tue Nov 17 19:55:15 2026] drbd p12 lima-dv-1: peer( Secondary -> Primary ) [remote]
[Tue Nov 17 19:55:23 2026] drbd p12: Preparing remote state change 2351429808: 2->0 conn( Disconnecting ) disk( Outdated )
[Tue Nov 17 19:55:23 2026] drbd p12 lima-dv-3: Committing remote state change 2351429808 (primary_nodes=1)
[Tue Nov 17 19:55:23 2026] drbd p12/0 drbd1012 lima-dv-3: pdsk( UpToDate -> Outdated ) [peer-state]
[Tue Nov 17 19:55:23 2026] drbd p12/0 drbd1012 lima-dv-1: received new current UUID: 4AF2630D459D876B weak_nodes=FFFFFFFFFFFFFFFC
[Tue Nov 17 19:55:23 2026] drbd p12: Preparing remote state change 4205220973: 2->1 conn( Disconnecting )
[Tue Nov 17 19:55:23 2026] drbd p12 lima-dv-3: Committing remote state change 4205220973 (primary_nodes=1)
[Tue Nov 17 19:55:23 2026] drbd p12 lima-dv-3: conn( Connected -> TearDown ) peer( Secondary -> Unknown ) [remote]
[Tue Nov 17 19:55:23 2026] drbd p12/0 drbd1012 lima-dv-3: repl( Established -> Off ) [remote]
[Tue Nov 17 19:55:23 2026] drbd p12 lima-dv-3: Terminating sender thread
[Tue Nov 17 19:55:23 2026] drbd p12 lima-dv-3: Starting sender thread (peer-node-id 2)
[Tue Nov 17 19:55:23 2026] drbd p12 lima-dv-3: Connection closed
[Tue Nov 17 19:55:23 2026] drbd p12 lima-dv-3: conn( TearDown -> Unconnected ) [disconnected]
[Tue Nov 17 19:55:23 2026] drbd p12 lima-dv-3: Restarting receiver thread
[Tue Nov 17 19:55:23 2026] drbd p12 lima-dv-3: conn( Unconnected -> Connecting ) [connecting]
[Tue Nov 17 19:55:28 2026] drbd p12/0 drbd1012: md_sync_timer expired! Worker calls drbd_md_sync().
[Tue Nov 17 19:56:05 2026] drbd p12 lima-dv-3: conn( Connecting -> Disconnecting ) [del-peer]
[Tue Nov 17 19:56:05 2026] drbd p12 lima-dv-3: Terminating sender thread
[Tue Nov 17 19:56:05 2026] drbd p12 lima-dv-3: Starting sender thread (peer-node-id 2)
[Tue Nov 17 19:56:05 2026] drbd p12 lima-dv-3: Connection closed
[Tue Nov 17 19:56:05 2026] drbd p12 lima-dv-3: conn( Disconnecting -> StandAlone ) [disconnected]
[Tue Nov 17 19:56:05 2026] drbd p12 lima-dv-3: Terminating receiver thread
[Tue Nov 17 19:56:05 2026] drbd p12 lima-dv-3: Terminating sender thread
[Tue Nov 17 19:56:25 2026] drbd p12: Preparing remote state change 53499271: 0->all role( Secondary )
[Tue Nov 17 19:56:25 2026] drbd p12 lima-dv-1: Committing remote state change 53499271 (primary_nodes=0)
[Tue Nov 17 19:56:25 2026] drbd p12 lima-dv-1: peer( Primary -> Secondary ) [remote]

### P12 step 6: a diskless node leaves, adjust on the others first
$ [dv-1] drbdadm create-md --force --max-peers=7 p12
initializing bitmap (224 KB) to all zero
initializing activity log
Writing meta data...
New drbd meta data block successfully created.
rc=0
$ [dv-2] drbdadm create-md --force --max-peers=7 p12
initializing activity log
Writing meta data...
New drbd meta data block successfully created.
initializing bitmap (224 KB) to all zero
rc=0
$ [dv-1] drbdadm up p12
rc=0
$ [dv-2] drbdadm up p12
rc=0
$ [dv-4] drbdadm up p12
rc=0
wait: dv-1: 2 connections Connected after 2 s at most
wait: dv-2: 2 connections Connected after 2 s at most
wait: dv-4: 2 connections Connected after 2 s at most
$ [dv-1] drbdadm new-current-uuid --clear-bitmap p12/0
rc=0
$ [dv-1] drbdsetup status p12 --verbose
p12 node-id:0 role:Secondary suspended:no force-io-failures:no
  volume:0 minor:1012 disk:UpToDate backing_dev:/dev/ambar-exp/p12 quorum:yes
      open:no blocked:no
  lima-dv-2 node-id:1 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
  lima-dv-4 node-id:3 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:Diskless peer-client:yes
        resync-suspended:no

rc=0
$ [dv-1] mkfs.ext4 -q -F -E nodiscard /dev/drbd1012
rc=0
19:57:51.760
946
started

### P12 step 6a: p12.res without dv-4 on dv-1 and dv-2, adjust while dv-4 is up and connected
@ 19:57:59.386 adjust on dv-1
$ [dv-1] drbdadm adjust p12
rc=0
$ [dv-2] drbdadm adjust p12
rc=0
$ [dv-1] drbdsetup status p12 --verbose --statistics
p12 node-id:0 role:Primary suspended:no force-io-failures:no
    write-ordering:flush
  volume:0 minor:1012 disk:UpToDate backing_dev:/dev/ambar-exp/p12 quorum:yes
      open:yes
      size:1048316 read:1005 written:83096 al-writes:9 bm-writes:65 upper-pending:2
      lower-pending:0 al-suspended:no blocked:no
  lima-dv-2 node-id:1 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:24 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
        received:0 sent:50200 out-of-sync:0 pending:2 unacked:0

rc=0
$ [dv-2] drbdsetup status p12 --verbose --statistics
p12 node-id:1 role:Secondary suspended:no force-io-failures:no
    write-ordering:flush
  volume:0 minor:1012 disk:UpToDate backing_dev:/dev/ambar-exp/p12 quorum:yes
      open:no
      size:1048316 read:0 written:86484 al-writes:10 bm-writes:65 upper-pending:0
      lower-pending:1 al-suspended:no blocked:no
  lima-dv-1 node-id:0 connection:Connected role:Primary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
        received:53592 sent:0 out-of-sync:0 pending:0 unacked:1

rc=0
$ [dv-4] drbdsetup status p12 --verbose --statistics
p12 node-id:3 role:Secondary suspended:no force-io-failures:no
    write-ordering:none
  volume:0 minor:1012 disk:Diskless client:yes backing_dev:none quorum:no open:no
      size:1048316 read:0 written:0 al-writes:0 bm-writes:0 upper-pending:0
      lower-pending:0 al-suspended:no blocked:no
  lima-dv-1 node-id:0 connection:Connecting role:Unknown tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Off peer-disk:DUnknown resync-suspended:no
        received:0 sent:0 out-of-sync:0 pending:0 unacked:0
  lima-dv-2 node-id:1 connection:Connecting role:Unknown tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Off peer-disk:DUnknown resync-suspended:no
        received:0 sent:0 out-of-sync:0 pending:0 unacked:0

rc=0
@ 19:58:20.132 state after adjust

### P12 step 6b: down on dv-4
@ 19:58:20.323 down on dv-4
$ [dv-4] drbdadm down p12
rc=0
$ [dv-1] drbdsetup status p12 --verbose --statistics
p12 node-id:0 role:Primary suspended:no force-io-failures:no
    write-ordering:flush
  volume:0 minor:1012 disk:UpToDate backing_dev:/dev/ambar-exp/p12 quorum:yes
      open:yes
      size:1048316 read:1005 written:135840 al-writes:9 bm-writes:65 upper-pending:0
      lower-pending:0 al-suspended:no blocked:no
  lima-dv-2 node-id:1 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
        received:0 sent:102944 out-of-sync:0 pending:0 unacked:0

rc=0
$ [dv-2] drbdsetup status p12 --verbose --statistics
p12 node-id:1 role:Secondary suspended:no force-io-failures:no
    write-ordering:flush
  volume:0 minor:1012 disk:UpToDate backing_dev:/dev/ambar-exp/p12 quorum:yes
      open:no
      size:1048316 read:0 written:140144 al-writes:10 bm-writes:65 upper-pending:0
      lower-pending:1 al-suspended:no blocked:no
  lima-dv-1 node-id:0 connection:Connected role:Primary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
        received:107252 sent:0 out-of-sync:0 pending:0 unacked:1

rc=0
$ [dv-4] drbdsetup status p12 --verbose
# No currently configured DRBD found.
p12: No such resource
rc=10
@ 19:58:36.666 stop the writer
acked=6979 on-volume=6980
--- probe.sh log (stdout and stderr)
--- watch.sh log
samples=95 probe=alive:R=4 probe=alive:S=91 longest-gap-without-new-acked=1.0s (from 19:57:54.633)
--- quorum log of dv-1 (runs of equal values: first - last  value  samples)
19:57:57.100 - 19:58:37.636  "quorum": true  x41
--- dmesg -T | grep 'drbd p12' | tail -n 80 [dv-1]
[Tue Nov 17 19:57:27 2026] drbd p12: Method to ensure write ordering: flush
[Tue Nov 17 19:57:27 2026] drbd p12/0 drbd1012: Allocating 224 KiB for new bitmap
[Tue Nov 17 19:57:27 2026] drbd p12/0 drbd1012: resync bitmap: bits=262079 bits_4k=262079 words=28665 pages=56
[Tue Nov 17 19:57:27 2026] drbd p12/0 drbd1012: size = 1024 MB (1048316 KB)
[Tue Nov 17 19:57:27 2026] drbd p12/0 drbd1012: Suspended AL updates
[Tue Nov 17 19:57:27 2026] drbd p12/0 drbd1012: disk( Attaching -> Inconsistent ) [attach]
[Tue Nov 17 19:57:27 2026] drbd p12/0 drbd1012: attached to current UUID: 0000000000000004
[Tue Nov 17 19:57:27 2026] drbd p12 lima-dv-2: conn( StandAlone -> Unconnected ) [connect]
[Tue Nov 17 19:57:27 2026] drbd p12 lima-dv-2: Starting receiver thread (peer-node-id 1)
[Tue Nov 17 19:57:27 2026] drbd p12 lima-dv-2: conn( Unconnected -> Connecting ) [connecting]
[Tue Nov 17 19:57:27 2026] drbd p12 lima-dv-4: conn( StandAlone -> Unconnected ) [connect]
[Tue Nov 17 19:57:27 2026] drbd p12 lima-dv-4: Starting receiver thread (peer-node-id 3)
[Tue Nov 17 19:57:27 2026] drbd p12 lima-dv-4: conn( Unconnected -> Connecting ) [connecting]
[Tue Nov 17 19:57:29 2026] drbd p12 lima-dv-2: Handshake to peer 1 successful: Agreed network protocol version 125
[Tue Nov 17 19:57:29 2026] drbd p12 lima-dv-2: Feature flags enabled on protocol level: 0x7ff TRIM THIN_RESYNC WRITE_SAME WRITE_ZEROES RESYNC_DAGTAG WRITE_POSTPONE
[Tue Nov 17 19:57:29 2026] drbd p12: Preparing cluster-wide state change 189311018: 0->1 role( Secondary ) conn( Connected )
[Tue Nov 17 19:57:29 2026] drbd p12/0 drbd1012 lima-dv-2: drbd_sync_handshake:
[Tue Nov 17 19:57:29 2026] drbd p12/0 drbd1012 lima-dv-2: self 0000000000000004:0000000000000000:0000000000000000:0000000000000000 bits:262079 flags:24
[Tue Nov 17 19:57:29 2026] drbd p12/0 drbd1012 lima-dv-2: peer 0000000000000004:0000000000000000:0000000000000000:0000000000000000 bits:262079 flags:24
[Tue Nov 17 19:57:29 2026] drbd p12/0 drbd1012 lima-dv-2: uuid_compare()=no-sync by rule=just-created-both
[Tue Nov 17 19:57:29 2026] drbd p12: State change 189311018: primary_nodes=0, weak_nodes=0
[Tue Nov 17 19:57:29 2026] drbd p12: Committing cluster-wide state change 189311018 (16ms)
[Tue Nov 17 19:57:29 2026] drbd p12 lima-dv-2: conn( Connecting -> Connected ) peer( Unknown -> Secondary ) [connected]
[Tue Nov 17 19:57:29 2026] drbd p12/0 drbd1012: quorum( no -> yes ) [connected]
[Tue Nov 17 19:57:29 2026] drbd p12/0 drbd1012 lima-dv-2: pdsk( DUnknown -> Inconsistent ) repl( Off -> Established ) replication( yes -> no ) peer_replication( yes -> no ) [connected]
[Tue Nov 17 19:57:29 2026] drbd p12/0 drbd1012: Resumed AL updates
[Tue Nov 17 19:57:30 2026] drbd p12: Preparing remote state change 2728758467: 1->3 role( Secondary ) conn( Connected )
[Tue Nov 17 19:57:30 2026] drbd p12 lima-dv-4: Handshake to peer 3 successful: Agreed network protocol version 125
[Tue Nov 17 19:57:30 2026] drbd p12 lima-dv-4: Feature flags enabled on protocol level: 0x7ff TRIM THIN_RESYNC WRITE_SAME WRITE_ZEROES RESYNC_DAGTAG WRITE_POSTPONE
[Tue Nov 17 19:57:30 2026] drbd p12 lima-dv-2: Committing remote state change 2728758467 (primary_nodes=0)
[Tue Nov 17 19:57:30 2026] drbd p12: Preparing cluster-wide state change 3961225815: 0->3 role( Secondary ) conn( Connected )
[Tue Nov 17 19:57:30 2026] drbd p12/0 drbd1012 lima-dv-4: self 0000000000000004:0000000000000000:0000000000000000:0000000000000000 bits:0 flags:0
[Tue Nov 17 19:57:30 2026] drbd p12/0 drbd1012 lima-dv-4: peer's exposed UUID: 0000000000000000
[Tue Nov 17 19:57:30 2026] drbd p12: State change 3961225815: primary_nodes=0, weak_nodes=0
[Tue Nov 17 19:57:30 2026] drbd p12: Committing cluster-wide state change 3961225815 (24ms)
[Tue Nov 17 19:57:30 2026] drbd p12 lima-dv-4: conn( Connecting -> Connected ) peer( Unknown -> Secondary ) [connected]
[Tue Nov 17 19:57:30 2026] drbd p12/0 drbd1012 lima-dv-4: pdsk( DUnknown -> Diskless ) repl( Off -> Established ) peer_replication( yes -> no ) [connected]
[Tue Nov 17 19:57:38 2026] drbd p12/0 drbd1012 lima-dv-2: Preparing to skip initial sync
[Tue Nov 17 19:57:38 2026] drbd p12/0 drbd1012: Bitmap UUID for initial resync: 7394624F0B1E9A68
[Tue Nov 17 19:57:38 2026] drbd p12/0 drbd1012: new current UUID: AB82BC22D5C387A6 weak: FFFFFFFFFFFFFFFD
[Tue Nov 17 19:57:38 2026] drbd p12/0 drbd1012 lima-dv-4: cleared bitmap UUID AB82BC22D5C387A6:0000000000000000:0000000000000000:0000000000000000
[Tue Nov 17 19:57:38 2026] drbd p12/0 drbd1012 lima-dv-2: cleared bitmap UUID AB82BC22D5C387A6:0000000000000000:0000000000000000:0000000000000000
[Tue Nov 17 19:57:38 2026] drbd p12/0 drbd1012: disk( Inconsistent -> UpToDate ) [new-c-uuid]
[Tue Nov 17 19:57:38 2026] drbd p12/0 drbd1012 lima-dv-2: pdsk( Inconsistent -> UpToDate ) replication( no -> yes ) [new-c-uuid]
[Tue Nov 17 19:57:38 2026] drbd p12/0 drbd1012: persisting effective size = 1024 MB (1048316 KB)
[Tue Nov 17 19:57:44 2026] drbd p12: Preparing cluster-wide state change 3125187760: 0->all role( Primary )
[Tue Nov 17 19:57:44 2026] drbd p12: State change 3125187760: primary_nodes=1, weak_nodes=FFFFFFFFFFFFFFF4
[Tue Nov 17 19:57:44 2026] drbd p12: Committing cluster-wide state change 3125187760 (0ms)
[Tue Nov 17 19:57:44 2026] drbd p12: role( Secondary -> Primary ) [mkfs.ext4:299183 auto-promote]
[Tue Nov 17 19:57:44 2026] drbd p12/0 drbd1012: gen-obligation: NONE -> ARMED(promoted)
[Tue Nov 17 19:57:44 2026] drbd p12/0 drbd1012: gen-obligation: ARMED(promoted) -> MINTING(promoted)
[Tue Nov 17 19:57:44 2026] drbd p12/0 drbd1012: gen-obligation: MINTING(promoted) -> DISCHARGED(promoted)
[Tue Nov 17 19:57:44 2026] drbd p12: Preparing cluster-wide state change 1572212466: 0->all role( Secondary )
[Tue Nov 17 19:57:44 2026] drbd p12: State change 1572212466: primary_nodes=0, weak_nodes=0
[Tue Nov 17 19:57:44 2026] drbd p12: Committing cluster-wide state change 1572212466 (0ms)
[Tue Nov 17 19:57:44 2026] drbd p12: role( Primary -> Secondary ) [mkfs.ext4:299183 auto-demote]
[Tue Nov 17 19:57:44 2026] drbd p12: Preparing cluster-wide state change 2113764521: 0->all role( Primary )
[Tue Nov 17 19:57:44 2026] drbd p12: State change 2113764521: primary_nodes=1, weak_nodes=FFFFFFFFFFFFFFF4
[Tue Nov 17 19:57:44 2026] drbd p12: Committing cluster-wide state change 2113764521 (4ms)
[Tue Nov 17 19:57:44 2026] drbd p12: role( Secondary -> Primary ) [mount:299208 auto-promote]
[Tue Nov 17 19:57:44 2026] drbd p12/0 drbd1012: gen-obligation: DISCHARGED(promoted) -> ARMED(promoted)
[Tue Nov 17 19:57:44 2026] drbd p12/0 drbd1012: gen-obligation: ARMED(promoted) -> MINTING(promoted)
[Tue Nov 17 19:57:44 2026] drbd p12/0 drbd1012: gen-obligation: MINTING(promoted) -> DISCHARGED(promoted)
[Tue Nov 17 19:57:59 2026] drbd p12: Preparing cluster-wide state change 424661837: 0->3 conn( Disconnecting ) pdsk( Outdated )
[Tue Nov 17 19:57:59 2026] drbd p12: State change 424661837: primary_nodes=1, weak_nodes=FFFFFFFFFFFFFFFC
[Tue Nov 17 19:57:59 2026] drbd p12: Committing cluster-wide state change 424661837 (0ms)
[Tue Nov 17 19:57:59 2026] drbd p12 lima-dv-4: conn( Connected -> Disconnecting ) peer( Secondary -> Unknown ) [del-peer]
[Tue Nov 17 19:57:59 2026] drbd p12/0 drbd1012 lima-dv-4: pdsk( Diskless -> DUnknown ) repl( Established -> Off ) [del-peer]
[Tue Nov 17 19:57:59 2026] drbd p12 lima-dv-4: Terminating sender thread
[Tue Nov 17 19:57:59 2026] drbd p12 lima-dv-4: Starting sender thread (peer-node-id 3)
[Tue Nov 17 19:57:59 2026] drbd p12 lima-dv-4: Connection closed
[Tue Nov 17 19:57:59 2026] drbd p12 lima-dv-4: conn( Disconnecting -> StandAlone ) [disconnected]
[Tue Nov 17 19:57:59 2026] drbd p12 lima-dv-4: Terminating receiver thread
[Tue Nov 17 19:57:59 2026] drbd p12 lima-dv-4: Terminating sender thread
[Tue Nov 17 19:58:01 2026] drbd p12: Preparing remote state change 451120797: 1->3 conn( Disconnecting )
[Tue Nov 17 19:58:01 2026] drbd p12 lima-dv-2: Committing remote state change 451120797 (primary_nodes=1)
[Tue Nov 17 19:58:36 2026] drbd p12: Preparing cluster-wide state change 1953616839: 0->all role( Secondary )
[Tue Nov 17 19:58:36 2026] drbd p12: State change 1953616839: primary_nodes=0, weak_nodes=0
[Tue Nov 17 19:58:36 2026] drbd p12: Committing cluster-wide state change 1953616839 (4ms)
[Tue Nov 17 19:58:36 2026] drbd p12: role( Primary -> Secondary ) [umount:308464 auto-demote]
--- dmesg -T | grep 'drbd p12' | tail -n 80 [dv-2]
[Tue Nov 17 19:56:41 2026] drbd p12 lima-dv-1: Connection closed
[Tue Nov 17 19:56:41 2026] drbd p12 lima-dv-1: conn( Disconnecting -> StandAlone ) [disconnected]
[Tue Nov 17 19:56:41 2026] drbd p12 lima-dv-1: Terminating receiver thread
[Tue Nov 17 19:56:41 2026] drbd p12 lima-dv-1: Terminating sender thread
[Tue Nov 17 19:56:41 2026] drbd p12/0 drbd1012: disk( UpToDate -> Detaching ) [down]
[Tue Nov 17 19:56:41 2026] drbd p12/0 drbd1012: disk( Detaching -> Diskless ) [go-diskless]
[Tue Nov 17 19:56:41 2026] drbd p12/0 drbd1012: Freeing bitmap of size 224 KiB
[Tue Nov 17 19:57:29 2026] drbd p12: Starting worker thread (node-id 1)
[Tue Nov 17 19:57:29 2026] drbd p12 lima-dv-1: Starting sender thread (peer-node-id 0)
[Tue Nov 17 19:57:29 2026] drbd p12 lima-dv-4: Starting sender thread (peer-node-id 3)
[Tue Nov 17 19:57:29 2026] drbd p12/0 drbd1012: meta-data IO uses: blk-bio
[Tue Nov 17 19:57:29 2026] drbd p12/0 drbd1012: disk( Diskless -> Attaching ) [attach]
[Tue Nov 17 19:57:29 2026] drbd p12/0 drbd1012: Maximum number of peer devices = 7
[Tue Nov 17 19:57:29 2026] drbd p12: Method to ensure write ordering: flush
[Tue Nov 17 19:57:29 2026] drbd p12/0 drbd1012: Allocating 224 KiB for new bitmap
[Tue Nov 17 19:57:29 2026] drbd p12/0 drbd1012: resync bitmap: bits=262079 bits_4k=262079 words=28665 pages=56
[Tue Nov 17 19:57:29 2026] drbd p12/0 drbd1012: size = 1024 MB (1048316 KB)
[Tue Nov 17 19:57:29 2026] drbd p12/0 drbd1012: Suspended AL updates
[Tue Nov 17 19:57:29 2026] drbd p12/0 drbd1012: disk( Attaching -> Inconsistent ) [attach]
[Tue Nov 17 19:57:29 2026] drbd p12/0 drbd1012: attached to current UUID: 0000000000000004
[Tue Nov 17 19:57:29 2026] drbd p12 lima-dv-1: conn( StandAlone -> Unconnected ) [connect]
[Tue Nov 17 19:57:29 2026] drbd p12 lima-dv-1: Starting receiver thread (peer-node-id 0)
[Tue Nov 17 19:57:29 2026] drbd p12 lima-dv-4: conn( StandAlone -> Unconnected ) [connect]
[Tue Nov 17 19:57:29 2026] drbd p12 lima-dv-1: conn( Unconnected -> Connecting ) [connecting]
[Tue Nov 17 19:57:29 2026] drbd p12 lima-dv-4: Starting receiver thread (peer-node-id 3)
[Tue Nov 17 19:57:29 2026] drbd p12 lima-dv-4: conn( Unconnected -> Connecting ) [connecting]
[Tue Nov 17 19:57:29 2026] drbd p12 lima-dv-1: Handshake to peer 0 successful: Agreed network protocol version 125
[Tue Nov 17 19:57:29 2026] drbd p12 lima-dv-1: Feature flags enabled on protocol level: 0x7ff TRIM THIN_RESYNC WRITE_SAME WRITE_ZEROES RESYNC_DAGTAG WRITE_POSTPONE
[Tue Nov 17 19:57:29 2026] drbd p12: Preparing remote state change 189311018: 0->1 role( Secondary ) conn( Connected )
[Tue Nov 17 19:57:29 2026] drbd p12/0 drbd1012 lima-dv-1: drbd_sync_handshake:
[Tue Nov 17 19:57:29 2026] drbd p12/0 drbd1012 lima-dv-1: self 0000000000000004:0000000000000000:0000000000000000:0000000000000000 bits:262079 flags:24
[Tue Nov 17 19:57:29 2026] drbd p12/0 drbd1012 lima-dv-1: peer 0000000000000004:0000000000000000:0000000000000000:0000000000000000 bits:262079 flags:24
[Tue Nov 17 19:57:29 2026] drbd p12/0 drbd1012 lima-dv-1: uuid_compare()=no-sync by rule=just-created-both
[Tue Nov 17 19:57:29 2026] drbd p12 lima-dv-1: Committing remote state change 189311018 (primary_nodes=0)
[Tue Nov 17 19:57:29 2026] drbd p12 lima-dv-1: conn( Connecting -> Connected ) peer( Unknown -> Secondary ) [remote]
[Tue Nov 17 19:57:29 2026] drbd p12/0 drbd1012: quorum( no -> yes ) [remote]
[Tue Nov 17 19:57:29 2026] drbd p12/0 drbd1012 lima-dv-1: pdsk( DUnknown -> Inconsistent ) repl( Off -> Established ) replication( yes -> no ) peer_replication( yes -> no ) [remote]
[Tue Nov 17 19:57:29 2026] drbd p12/0 drbd1012: Resumed AL updates
[Tue Nov 17 19:57:31 2026] drbd p12 lima-dv-4: Handshake to peer 3 successful: Agreed network protocol version 125
[Tue Nov 17 19:57:31 2026] drbd p12 lima-dv-4: Feature flags enabled on protocol level: 0x7ff TRIM THIN_RESYNC WRITE_SAME WRITE_ZEROES RESYNC_DAGTAG WRITE_POSTPONE
[Tue Nov 17 19:57:31 2026] drbd p12: Preparing cluster-wide state change 2728758467: 1->3 role( Secondary ) conn( Connected )
[Tue Nov 17 19:57:31 2026] drbd p12/0 drbd1012 lima-dv-4: self 0000000000000004:0000000000000000:0000000000000000:0000000000000000 bits:0 flags:0
[Tue Nov 17 19:57:31 2026] drbd p12/0 drbd1012 lima-dv-4: peer's exposed UUID: 0000000000000000
[Tue Nov 17 19:57:31 2026] drbd p12: State change 2728758467: primary_nodes=0, weak_nodes=0
[Tue Nov 17 19:57:31 2026] drbd p12: Committing cluster-wide state change 2728758467 (20ms)
[Tue Nov 17 19:57:31 2026] drbd p12 lima-dv-4: conn( Connecting -> Connected ) peer( Unknown -> Secondary ) [connected]
[Tue Nov 17 19:57:31 2026] drbd p12/0 drbd1012 lima-dv-4: pdsk( DUnknown -> Diskless ) repl( Off -> Established ) peer_replication( yes -> no ) [connected]
[Tue Nov 17 19:57:31 2026] drbd p12: Preparing remote state change 3961225815: 0->3 role( Secondary ) conn( Connected )
[Tue Nov 17 19:57:31 2026] drbd p12 lima-dv-1: Committing remote state change 3961225815 (primary_nodes=0)
[Tue Nov 17 19:57:38 2026] drbd p12/0 drbd1012: Accepted new current UUID, preparing to skip initial sync
[Tue Nov 17 19:57:38 2026] drbd p12/0 drbd1012: disk( Inconsistent -> UpToDate ) [skip-initial-sync]
[Tue Nov 17 19:57:38 2026] drbd p12/0 drbd1012 lima-dv-1: pdsk( Inconsistent -> UpToDate ) replication( no -> yes ) [skip-initial-sync]
[Tue Nov 17 19:57:38 2026] drbd p12/0 drbd1012: persisting effective size = 1024 MB (1048316 KB)
[Tue Nov 17 19:57:38 2026] drbd p12/0 drbd1012 lima-dv-1: receiver updated UUIDs to AB82BC22D5C387A6:0000000000000000:0000000000000000:0000000000000000
[Tue Nov 17 19:57:44 2026] drbd p12: Preparing remote state change 3125187760: 0->all role( Primary )
[Tue Nov 17 19:57:44 2026] drbd p12 lima-dv-1: Committing remote state change 3125187760 (primary_nodes=1)
[Tue Nov 17 19:57:44 2026] drbd p12 lima-dv-1: peer( Secondary -> Primary ) [remote]
[Tue Nov 17 19:57:44 2026] drbd p12: Preparing remote state change 1572212466: 0->all role( Secondary )
[Tue Nov 17 19:57:44 2026] drbd p12 lima-dv-1: Committing remote state change 1572212466 (primary_nodes=0)
[Tue Nov 17 19:57:44 2026] drbd p12 lima-dv-1: peer( Primary -> Secondary ) [remote]
[Tue Nov 17 19:57:44 2026] drbd p12: Preparing remote state change 2113764521: 0->all role( Primary )
[Tue Nov 17 19:57:44 2026] drbd p12 lima-dv-1: Committing remote state change 2113764521 (primary_nodes=1)
[Tue Nov 17 19:57:44 2026] drbd p12 lima-dv-1: peer( Secondary -> Primary ) [remote]
[Tue Nov 17 19:57:59 2026] drbd p12: Preparing remote state change 424661837: 0->3 conn( Disconnecting ) pdsk( Outdated )
[Tue Nov 17 19:57:59 2026] drbd p12 lima-dv-1: Committing remote state change 424661837 (primary_nodes=1)
[Tue Nov 17 19:58:01 2026] drbd p12: Preparing cluster-wide state change 451120797: 1->3 conn( Disconnecting )
[Tue Nov 17 19:58:02 2026] drbd p12: State change 451120797: primary_nodes=1, weak_nodes=FFFFFFFFFFFFFFFE
[Tue Nov 17 19:58:02 2026] drbd p12 lima-dv-4: Cluster is now split
[Tue Nov 17 19:58:02 2026] drbd p12: Committing cluster-wide state change 451120797 (20ms)
[Tue Nov 17 19:58:02 2026] drbd p12 lima-dv-4: conn( Connected -> Disconnecting ) peer( Secondary -> Unknown ) [del-peer]
[Tue Nov 17 19:58:02 2026] drbd p12/0 drbd1012 lima-dv-4: pdsk( Diskless -> DUnknown ) repl( Established -> Off ) [del-peer]
[Tue Nov 17 19:58:02 2026] drbd p12 lima-dv-4: Terminating sender thread
[Tue Nov 17 19:58:02 2026] drbd p12 lima-dv-4: Starting sender thread (peer-node-id 3)
[Tue Nov 17 19:58:02 2026] drbd p12 lima-dv-4: Connection closed
[Tue Nov 17 19:58:02 2026] drbd p12 lima-dv-4: conn( Disconnecting -> StandAlone ) [disconnected]
[Tue Nov 17 19:58:02 2026] drbd p12 lima-dv-4: Terminating receiver thread
[Tue Nov 17 19:58:02 2026] drbd p12 lima-dv-4: Terminating sender thread
[Tue Nov 17 19:58:36 2026] drbd p12: Preparing remote state change 1953616839: 0->all role( Secondary )
[Tue Nov 17 19:58:36 2026] drbd p12 lima-dv-1: Committing remote state change 1953616839 (primary_nodes=0)
[Tue Nov 17 19:58:36 2026] drbd p12 lima-dv-1: peer( Primary -> Secondary ) [remote]
--- dmesg -T | grep 'drbd p12' | tail -n 80 [dv-4]
[Tue Nov 17 19:57:30 2026] drbd p12: Starting worker thread (node-id 3)
[Tue Nov 17 19:57:30 2026] drbd p12 lima-dv-1: Starting sender thread (peer-node-id 0)
[Tue Nov 17 19:57:30 2026] drbd p12 lima-dv-2: Starting sender thread (peer-node-id 1)
[Tue Nov 17 19:57:30 2026] drbd p12 lima-dv-1: conn( StandAlone -> Unconnected ) [connect]
[Tue Nov 17 19:57:30 2026] drbd p12 lima-dv-1: Starting receiver thread (peer-node-id 0)
[Tue Nov 17 19:57:30 2026] drbd p12 lima-dv-1: conn( Unconnected -> Connecting ) [connecting]
[Tue Nov 17 19:57:30 2026] drbd p12 lima-dv-2: conn( StandAlone -> Unconnected ) [connect]
[Tue Nov 17 19:57:30 2026] drbd p12 lima-dv-2: Starting receiver thread (peer-node-id 1)
[Tue Nov 17 19:57:30 2026] drbd p12 lima-dv-2: conn( Unconnected -> Connecting ) [connecting]
[Tue Nov 17 19:57:31 2026] drbd p12 lima-dv-2: Handshake to peer 1 successful: Agreed network protocol version 125
[Tue Nov 17 19:57:31 2026] drbd p12 lima-dv-2: Feature flags enabled on protocol level: 0x7ff TRIM THIN_RESYNC WRITE_SAME WRITE_ZEROES RESYNC_DAGTAG WRITE_POSTPONE
[Tue Nov 17 19:57:31 2026] drbd p12: Preparing remote state change 2728758467: 1->3 role( Secondary ) conn( Connected )
[Tue Nov 17 19:57:31 2026] drbd p12 lima-dv-1: Handshake to peer 0 successful: Agreed network protocol version 125
[Tue Nov 17 19:57:31 2026] drbd p12 lima-dv-1: Feature flags enabled on protocol level: 0x7ff TRIM THIN_RESYNC WRITE_SAME WRITE_ZEROES RESYNC_DAGTAG WRITE_POSTPONE
[Tue Nov 17 19:57:31 2026] drbd p12/0 drbd1012: size = 1024 MB (1048316 KB)
[Tue Nov 17 19:57:31 2026] drbd p12/0 drbd1012 lima-dv-2: my exposed UUID: 0000000000000000
[Tue Nov 17 19:57:31 2026] drbd p12/0 drbd1012 lima-dv-2: peer 0000000000000004:FFFFFFFFFFFFFFFF:0000000000000000:0000000000000000 bits:0 flags:24
[Tue Nov 17 19:57:31 2026] drbd p12 lima-dv-2: Committing remote state change 2728758467 (primary_nodes=0)
[Tue Nov 17 19:57:31 2026] drbd p12 lima-dv-2: conn( Connecting -> Connected ) peer( Unknown -> Secondary ) [remote]
[Tue Nov 17 19:57:31 2026] drbd p12/0 drbd1012 lima-dv-2: pdsk( DUnknown -> Inconsistent ) repl( Off -> Established ) replication( yes -> no ) [remote]
[Tue Nov 17 19:57:31 2026] drbd p12: Preparing remote state change 3961225815: 0->3 role( Secondary ) conn( Connected )
[Tue Nov 17 19:57:31 2026] drbd p12/0 drbd1012 lima-dv-1: my exposed UUID: 0000000000000000
[Tue Nov 17 19:57:31 2026] drbd p12/0 drbd1012 lima-dv-1: peer 0000000000000004:FFFFFFFFFFFFFFFF:0000000000000000:0000000000000000 bits:0 flags:24
[Tue Nov 17 19:57:31 2026] drbd p12 lima-dv-1: Committing remote state change 3961225815 (primary_nodes=0)
[Tue Nov 17 19:57:31 2026] drbd p12 lima-dv-1: conn( Connecting -> Connected ) peer( Unknown -> Secondary ) [remote]
[Tue Nov 17 19:57:31 2026] drbd p12/0 drbd1012: quorum( no -> yes ) [remote]
[Tue Nov 17 19:57:31 2026] drbd p12/0 drbd1012 lima-dv-1: pdsk( DUnknown -> Inconsistent ) repl( Off -> Established ) replication( yes -> no ) [remote]
[Tue Nov 17 19:57:38 2026] drbd p12/0 drbd1012: Setting exposed data uuid: AB82BC22D5C387A6
[Tue Nov 17 19:57:38 2026] drbd p12/0 drbd1012 lima-dv-1: pdsk( Inconsistent -> UpToDate ) replication( no -> yes ) [peer-state]
[Tue Nov 17 19:57:38 2026] drbd p12/0 drbd1012 lima-dv-2: pdsk( Inconsistent -> UpToDate ) replication( no -> yes ) [peer-state]
[Tue Nov 17 19:57:44 2026] drbd p12: Preparing remote state change 3125187760: 0->all role( Primary )
[Tue Nov 17 19:57:44 2026] drbd p12 lima-dv-1: Committing remote state change 3125187760 (primary_nodes=1)
[Tue Nov 17 19:57:44 2026] drbd p12 lima-dv-1: peer( Secondary -> Primary ) [remote]
[Tue Nov 17 19:57:44 2026] drbd p12: Preparing remote state change 1572212466: 0->all role( Secondary )
[Tue Nov 17 19:57:44 2026] drbd p12 lima-dv-1: Committing remote state change 1572212466 (primary_nodes=0)
[Tue Nov 17 19:57:44 2026] drbd p12 lima-dv-1: peer( Primary -> Secondary ) [remote]
[Tue Nov 17 19:57:44 2026] drbd p12: Preparing remote state change 2113764521: 0->all role( Primary )
[Tue Nov 17 19:57:44 2026] drbd p12 lima-dv-1: Committing remote state change 2113764521 (primary_nodes=1)
[Tue Nov 17 19:57:44 2026] drbd p12 lima-dv-1: peer( Secondary -> Primary ) [remote]
[Tue Nov 17 19:57:59 2026] drbd p12: Preparing remote state change 424661837: 0->3 conn( Disconnecting ) pdsk( Outdated )
[Tue Nov 17 19:57:59 2026] drbd p12 lima-dv-1: Committing remote state change 424661837 (primary_nodes=1)
[Tue Nov 17 19:57:59 2026] drbd p12 lima-dv-1: conn( Connected -> TearDown ) peer( Primary -> Unknown ) [remote]
[Tue Nov 17 19:57:59 2026] drbd p12/0 drbd1012 lima-dv-1: pdsk( UpToDate -> DUnknown ) repl( Established -> Off ) [remote]
[Tue Nov 17 19:57:59 2026] drbd p12 lima-dv-1: Terminating sender thread
[Tue Nov 17 19:57:59 2026] drbd p12 lima-dv-1: Starting sender thread (peer-node-id 0)
[Tue Nov 17 19:57:59 2026] drbd p12 lima-dv-1: Connection closed
[Tue Nov 17 19:57:59 2026] drbd p12 lima-dv-1: conn( TearDown -> Unconnected ) [disconnected]
[Tue Nov 17 19:57:59 2026] drbd p12 lima-dv-1: Restarting receiver thread
[Tue Nov 17 19:57:59 2026] drbd p12 lima-dv-1: conn( Unconnected -> Connecting ) [connecting]
[Tue Nov 17 19:58:02 2026] drbd p12: Preparing remote state change 451120797: 1->3 conn( Disconnecting )
[Tue Nov 17 19:58:02 2026] drbd p12 lima-dv-2: Committing remote state change 451120797 (primary_nodes=1)
[Tue Nov 17 19:58:02 2026] drbd p12 lima-dv-2: conn( Connected -> TearDown ) peer( Secondary -> Unknown ) [remote]
[Tue Nov 17 19:58:02 2026] drbd p12/0 drbd1012: quorum( yes -> no ) [remote]
[Tue Nov 17 19:58:02 2026] drbd p12/0 drbd1012 lima-dv-2: pdsk( UpToDate -> DUnknown ) repl( Established -> Off ) [remote]
[Tue Nov 17 19:58:02 2026] drbd p12 lima-dv-2: Terminating sender thread
[Tue Nov 17 19:58:02 2026] drbd p12 lima-dv-2: Starting sender thread (peer-node-id 1)
[Tue Nov 17 19:58:02 2026] drbd p12 lima-dv-2: Connection closed
[Tue Nov 17 19:58:02 2026] drbd p12 lima-dv-2: conn( TearDown -> Unconnected ) [disconnected]
[Tue Nov 17 19:58:02 2026] drbd p12 lima-dv-2: Restarting receiver thread
[Tue Nov 17 19:58:02 2026] drbd p12 lima-dv-2: conn( Unconnected -> Connecting ) [connecting]
[Tue Nov 17 19:58:20 2026] drbd p12 lima-dv-1: conn( Connecting -> Disconnecting ) [down]
[Tue Nov 17 19:58:20 2026] drbd p12 lima-dv-1: Terminating sender thread
[Tue Nov 17 19:58:20 2026] drbd p12 lima-dv-1: Starting sender thread (peer-node-id 0)
[Tue Nov 17 19:58:20 2026] drbd p12 lima-dv-1: Connection closed
[Tue Nov 17 19:58:20 2026] drbd p12 lima-dv-1: conn( Disconnecting -> StandAlone ) [disconnected]
[Tue Nov 17 19:58:20 2026] drbd p12 lima-dv-1: Terminating receiver thread
[Tue Nov 17 19:58:20 2026] drbd p12 lima-dv-1: Terminating sender thread
[Tue Nov 17 19:58:20 2026] drbd p12 lima-dv-2: conn( Connecting -> Disconnecting ) [down]
[Tue Nov 17 19:58:20 2026] drbd p12 lima-dv-2: Terminating sender thread
[Tue Nov 17 19:58:20 2026] drbd p12 lima-dv-2: Starting sender thread (peer-node-id 1)
[Tue Nov 17 19:58:20 2026] drbd p12 lima-dv-2: Connection closed
[Tue Nov 17 19:58:20 2026] drbd p12 lima-dv-2: conn( Disconnecting -> StandAlone ) [disconnected]
[Tue Nov 17 19:58:20 2026] drbd p12 lima-dv-2: Terminating receiver thread
[Tue Nov 17 19:58:20 2026] drbd p12 lima-dv-2: Terminating sender thread
exit=0
```

### P13. Слоты битовой карты при смене сторон

Ресурс `p13`: `dv-1`, `dv-2` с копией, `--max-peers=1`, port 7013, minor 1013, 64 МиБ случайных данных.

```bash
stand/spike/pre-2b.sh p13
```

```text
### P13 run 1: no forget-peer
$ [dv-1] drbdadm create-md --force --max-peers=1 p13
initializing bitmap (32 KB) to all zero
initializing activity log
Writing meta data...
New drbd meta data block successfully created.
rc=0
$ [dv-2] drbdadm create-md --force --max-peers=1 p13
initializing bitmap (32 KB) to all zero
initializing activity log
Writing meta data...
New drbd meta data block successfully created.
rc=0
$ [dv-1] drbdadm up p13
rc=0
$ [dv-2] drbdadm up p13
rc=0
wait: dv-1: 1 connections Connected after 2 s at most
wait: dv-2: 1 connections Connected after 2 s at most
$ [dv-1] drbdadm new-current-uuid --clear-bitmap p13/0
rc=0
$ [dv-1] drbdsetup status p13 --verbose
p13 node-id:0 role:Secondary suspended:no force-io-failures:no
  volume:0 minor:1013 disk:UpToDate backing_dev:/dev/ambar-exp/p13 quorum:yes
      open:no blocked:no
  lima-dv-2 node-id:1 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no

rc=0
$ [dv-1] dd if=/dev/urandom of=/dev/drbd1013 bs=1M count=64 oflag=direct status=none
rc=0
$ [dv-1] drbdsetup status p13 --verbose --statistics
p13 node-id:0 role:Secondary suspended:no force-io-failures:no
    write-ordering:flush
  volume:0 minor:1013 disk:UpToDate backing_dev:/dev/ambar-exp/p13 quorum:yes
      open:no
      size:1048508 read:0 written:65536 al-writes:16 bm-writes:8 upper-pending:0
      lower-pending:0 al-suspended:no blocked:no
  lima-dv-2 node-id:1 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
        received:0 sent:65536 out-of-sync:0 pending:0 unacked:0

rc=0

### P13 step 1: dv-2 leaves (down on dv-2, p13.res without dv-2 on dv-1, adjust on dv-1)
@ 20:00:11.881 down on dv-2
$ [dv-2] drbdadm down p13
rc=0
$ [dv-1] drbdadm adjust p13
rc=0
$ [dv-1] drbdsetup status p13 --verbose
p13 node-id:0 role:Secondary suspended:no force-io-failures:no
  volume:0 minor:1013 disk:UpToDate backing_dev:/dev/ambar-exp/p13 quorum:yes
      open:no blocked:no

rc=0

### P13 step 2: dv-3 (node-id 2) joins with a copy
$ [dv-3] drbdadm create-md --force --max-peers=1 p13
initializing bitmap (32 KB) to all zero
initializing activity log
Writing meta data...
New drbd meta data block successfully created.
rc=0
$ [dv-3] drbdadm up p13
rc=0
@ 20:00:22.459 adjust on dv-1
$ [dv-1] drbdadm adjust p13
p13: Failure: (162) Invalid configuration request
Command 'drbdsetup connect p13 2' terminated with exit code 10
rc=1
$ [dv-1] drbdsetup status p13 --verbose --statistics
p13 node-id:0 role:Secondary suspended:no force-io-failures:no
    write-ordering:flush
  volume:0 minor:1013 disk:UpToDate backing_dev:/dev/ambar-exp/p13 quorum:yes
      open:no
      size:1048508 read:0 written:65536 al-writes:16 bm-writes:8 upper-pending:0
      lower-pending:0 al-suspended:no blocked:no
  lima-dv-3 node-id:2 connection:StandAlone role:Unknown tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Off peer-disk:DUnknown resync-suspended:no
        received:0 sent:0 out-of-sync:0 pending:0 unacked:0

rc=0
$ [dv-3] drbdsetup status p13 --verbose --statistics
p13 node-id:2 role:Secondary suspended:no force-io-failures:no
    write-ordering:flush
  volume:0 minor:1013 disk:Inconsistent backing_dev:/dev/ambar-exp/p13 quorum:no
      open:no
      size:1048508 read:0 written:0 al-writes:0 bm-writes:0 upper-pending:0
      lower-pending:0 al-suspended:yes blocked:no
  lima-dv-1 node-id:0 connection:Connecting role:Unknown tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Off peer-disk:DUnknown resync-suspended:no
        received:0 sent:0 out-of-sync:1048508 pending:0 unacked:0

rc=0
--- dmesg -T | grep 'drbd p13' | tail -n 80 [dv-1]
[Tue Nov 17 19:59:56 2026] drbd p13: Starting worker thread (node-id 0)
[Tue Nov 17 19:59:56 2026] drbd p13 lima-dv-2: Starting sender thread (peer-node-id 1)
[Tue Nov 17 19:59:56 2026] drbd p13/0 drbd1013: meta-data IO uses: blk-bio
[Tue Nov 17 19:59:56 2026] drbd p13/0 drbd1013: disk( Diskless -> Attaching ) [attach]
[Tue Nov 17 19:59:56 2026] drbd p13/0 drbd1013: Maximum number of peer devices = 1
[Tue Nov 17 19:59:56 2026] drbd p13: Method to ensure write ordering: flush
[Tue Nov 17 19:59:56 2026] drbd p13/0 drbd1013: Allocating 32 KiB for new bitmap
[Tue Nov 17 19:59:56 2026] drbd p13/0 drbd1013: resync bitmap: bits=262127 bits_4k=262127 words=4096 pages=8
[Tue Nov 17 19:59:56 2026] drbd p13/0 drbd1013: size = 1024 MB (1048508 KB)
[Tue Nov 17 19:59:56 2026] drbd p13/0 drbd1013: Suspended AL updates
[Tue Nov 17 19:59:56 2026] drbd p13/0 drbd1013: disk( Attaching -> Inconsistent ) [attach]
[Tue Nov 17 19:59:56 2026] drbd p13/0 drbd1013: attached to current UUID: 0000000000000004
[Tue Nov 17 19:59:56 2026] drbd p13 lima-dv-2: conn( StandAlone -> Unconnected ) [connect]
[Tue Nov 17 19:59:56 2026] drbd p13 lima-dv-2: Starting receiver thread (peer-node-id 1)
[Tue Nov 17 19:59:56 2026] drbd p13 lima-dv-2: conn( Unconnected -> Connecting ) [connecting]
[Tue Nov 17 19:59:57 2026] drbd p13 lima-dv-2: Handshake to peer 1 successful: Agreed network protocol version 125
[Tue Nov 17 19:59:57 2026] drbd p13 lima-dv-2: Feature flags enabled on protocol level: 0x7ff TRIM THIN_RESYNC WRITE_SAME WRITE_ZEROES RESYNC_DAGTAG WRITE_POSTPONE
[Tue Nov 17 19:59:57 2026] drbd p13: Preparing cluster-wide state change 3766067413: 0->1 role( Secondary ) conn( Connected )
[Tue Nov 17 19:59:57 2026] drbd p13/0 drbd1013 lima-dv-2: drbd_sync_handshake:
[Tue Nov 17 19:59:57 2026] drbd p13/0 drbd1013 lima-dv-2: self 0000000000000004:0000000000000000:0000000000000000:0000000000000000 bits:262127 flags:24
[Tue Nov 17 19:59:57 2026] drbd p13/0 drbd1013 lima-dv-2: peer 0000000000000004:0000000000000000:0000000000000000:0000000000000000 bits:262127 flags:24
[Tue Nov 17 19:59:57 2026] drbd p13/0 drbd1013 lima-dv-2: uuid_compare()=no-sync by rule=just-created-both
[Tue Nov 17 19:59:57 2026] drbd p13: State change 3766067413: primary_nodes=0, weak_nodes=0
[Tue Nov 17 19:59:57 2026] drbd p13: Committing cluster-wide state change 3766067413 (16ms)
[Tue Nov 17 19:59:57 2026] drbd p13 lima-dv-2: conn( Connecting -> Connected ) peer( Unknown -> Secondary ) [connected]
[Tue Nov 17 19:59:57 2026] drbd p13/0 drbd1013: quorum( no -> yes ) [connected]
[Tue Nov 17 19:59:57 2026] drbd p13/0 drbd1013 lima-dv-2: pdsk( DUnknown -> Inconsistent ) repl( Off -> Established ) replication( yes -> no ) peer_replication( yes -> no ) [connected]
[Tue Nov 17 19:59:57 2026] drbd p13/0 drbd1013: Resumed AL updates
[Tue Nov 17 20:00:03 2026] drbd p13/0 drbd1013 lima-dv-2: Preparing to skip initial sync
[Tue Nov 17 20:00:03 2026] drbd p13/0 drbd1013: Bitmap UUID for initial resync: 03184198FB78E7DD
[Tue Nov 17 20:00:03 2026] drbd p13/0 drbd1013: new current UUID: 4C02FE6748EB8B2C weak: FFFFFFFFFFFFFFFD
[Tue Nov 17 20:00:03 2026] drbd p13/0 drbd1013 lima-dv-2: cleared bitmap UUID 4C02FE6748EB8B2C:0000000000000000:0000000000000000:0000000000000000
[Tue Nov 17 20:00:03 2026] drbd p13/0 drbd1013: disk( Inconsistent -> UpToDate ) [new-c-uuid]
[Tue Nov 17 20:00:03 2026] drbd p13/0 drbd1013 lima-dv-2: pdsk( Inconsistent -> UpToDate ) replication( no -> yes ) [new-c-uuid]
[Tue Nov 17 20:00:03 2026] drbd p13/0 drbd1013: persisting effective size = 1024 MB (1048508 KB)
[Tue Nov 17 20:00:08 2026] drbd p13: Preparing cluster-wide state change 3697426417: 0->all role( Primary )
[Tue Nov 17 20:00:08 2026] drbd p13: State change 3697426417: primary_nodes=1, weak_nodes=FFFFFFFFFFFFFFFC
[Tue Nov 17 20:00:08 2026] drbd p13: Committing cluster-wide state change 3697426417 (0ms)
[Tue Nov 17 20:00:08 2026] drbd p13: role( Secondary -> Primary ) [dd:315322 auto-promote]
[Tue Nov 17 20:00:08 2026] drbd p13/0 drbd1013: gen-obligation: NONE -> ARMED(promoted)
[Tue Nov 17 20:00:08 2026] drbd p13/0 drbd1013: gen-obligation: ARMED(promoted) -> MINTING(promoted)
[Tue Nov 17 20:00:08 2026] drbd p13/0 drbd1013: gen-obligation: MINTING(promoted) -> DISCHARGED(promoted)
[Tue Nov 17 20:00:08 2026] drbd p13: Preparing cluster-wide state change 4182745138: 0->all role( Secondary )
[Tue Nov 17 20:00:08 2026] drbd p13: State change 4182745138: primary_nodes=0, weak_nodes=0
[Tue Nov 17 20:00:08 2026] drbd p13: Committing cluster-wide state change 4182745138 (0ms)
[Tue Nov 17 20:00:08 2026] drbd p13: role( Primary -> Secondary ) [dd:315322 auto-demote]
[Tue Nov 17 20:00:10 2026] drbd p13: Preparing remote state change 1863920746: 1->0 conn( Disconnecting )
[Tue Nov 17 20:00:10 2026] drbd p13 lima-dv-2: Committing remote state change 1863920746 (primary_nodes=0)
[Tue Nov 17 20:00:10 2026] drbd p13 lima-dv-2: conn( Connected -> TearDown ) peer( Secondary -> Unknown ) [remote]
[Tue Nov 17 20:00:10 2026] drbd p13/0 drbd1013 lima-dv-2: pdsk( UpToDate -> DUnknown ) repl( Established -> Off ) [remote]
[Tue Nov 17 20:00:10 2026] drbd p13 lima-dv-2: Terminating sender thread
[Tue Nov 17 20:00:10 2026] drbd p13 lima-dv-2: Starting sender thread (peer-node-id 1)
[Tue Nov 17 20:00:10 2026] drbd p13 lima-dv-2: Connection closed
[Tue Nov 17 20:00:10 2026] drbd p13 lima-dv-2: conn( TearDown -> Unconnected ) [disconnected]
[Tue Nov 17 20:00:10 2026] drbd p13 lima-dv-2: Restarting receiver thread
[Tue Nov 17 20:00:10 2026] drbd p13 lima-dv-2: conn( Unconnected -> Connecting ) [connecting]
[Tue Nov 17 20:00:12 2026] drbd p13 lima-dv-2: conn( Connecting -> Disconnecting ) [del-peer]
[Tue Nov 17 20:00:12 2026] drbd p13 lima-dv-2: Terminating sender thread
[Tue Nov 17 20:00:12 2026] drbd p13 lima-dv-2: Starting sender thread (peer-node-id 1)
[Tue Nov 17 20:00:12 2026] drbd p13 lima-dv-2: Connection closed
[Tue Nov 17 20:00:12 2026] drbd p13 lima-dv-2: conn( Disconnecting -> StandAlone ) [disconnected]
[Tue Nov 17 20:00:12 2026] drbd p13 lima-dv-2: Terminating receiver thread
[Tue Nov 17 20:00:12 2026] drbd p13 lima-dv-2: Terminating sender thread
[Tue Nov 17 20:00:21 2026] drbd p13 lima-dv-3: Starting sender thread (peer-node-id 2)
[Tue Nov 17 20:00:21 2026] drbd p13/0 drbd1013 lima-dv-3: Not enough free bitmap slots
--- dmesg -T | grep 'drbd p13' | tail -n 80 [dv-3]
[Tue Nov 17 20:00:19 2026] drbd p13: Starting worker thread (node-id 2)
[Tue Nov 17 20:00:19 2026] drbd p13 lima-dv-1: Starting sender thread (peer-node-id 0)
[Tue Nov 17 20:00:19 2026] drbd p13/0 drbd1013: meta-data IO uses: blk-bio
[Tue Nov 17 20:00:19 2026] drbd p13/0 drbd1013: disk( Diskless -> Attaching ) [attach]
[Tue Nov 17 20:00:19 2026] drbd p13/0 drbd1013: Maximum number of peer devices = 1
[Tue Nov 17 20:00:19 2026] drbd p13: Method to ensure write ordering: flush
[Tue Nov 17 20:00:19 2026] drbd p13/0 drbd1013: Allocating 32 KiB for new bitmap
[Tue Nov 17 20:00:19 2026] drbd p13/0 drbd1013: resync bitmap: bits=262127 bits_4k=262127 words=4096 pages=8
[Tue Nov 17 20:00:19 2026] drbd p13/0 drbd1013: size = 1024 MB (1048508 KB)
[Tue Nov 17 20:00:19 2026] drbd p13/0 drbd1013: Suspended AL updates
[Tue Nov 17 20:00:19 2026] drbd p13/0 drbd1013: disk( Attaching -> Inconsistent ) [attach]
[Tue Nov 17 20:00:19 2026] drbd p13/0 drbd1013: attached to current UUID: 0000000000000004
[Tue Nov 17 20:00:19 2026] drbd p13 lima-dv-1: conn( StandAlone -> Unconnected ) [connect]
[Tue Nov 17 20:00:19 2026] drbd p13 lima-dv-1: Starting receiver thread (peer-node-id 0)
[Tue Nov 17 20:00:19 2026] drbd p13 lima-dv-1: conn( Unconnected -> Connecting ) [connecting]

### P13 step 4: UpToDate on dv-3 (up to 120 s more), then the hashes
t+1 s [dv-3]
  volume:0 minor:1013 disk:Inconsistent backing_dev:/dev/ambar-exp/p13 quorum:no
  volume:0 replication:Off peer-disk:DUnknown resync-suspended:no
  received:0 sent:0 out-of-sync:1048508 pending:0 unacked:0
t+7 s [dv-3]
  volume:0 minor:1013 disk:Inconsistent backing_dev:/dev/ambar-exp/p13 quorum:no
  volume:0 replication:Off peer-disk:DUnknown resync-suspended:no
  received:0 sent:0 out-of-sync:1048508 pending:0 unacked:0
t+14 s [dv-3]
  volume:0 minor:1013 disk:Inconsistent backing_dev:/dev/ambar-exp/p13 quorum:no
  volume:0 replication:Off peer-disk:DUnknown resync-suspended:no
  received:0 sent:0 out-of-sync:1048508 pending:0 unacked:0
t+20 s [dv-3]
  volume:0 minor:1013 disk:Inconsistent backing_dev:/dev/ambar-exp/p13 quorum:no
  volume:0 replication:Off peer-disk:DUnknown resync-suspended:no
  received:0 sent:0 out-of-sync:1048508 pending:0 unacked:0
t+26 s [dv-3]
  volume:0 minor:1013 disk:Inconsistent backing_dev:/dev/ambar-exp/p13 quorum:no
  volume:0 replication:Off peer-disk:DUnknown resync-suspended:no
  received:0 sent:0 out-of-sync:1048508 pending:0 unacked:0
t+32 s [dv-3]
  volume:0 minor:1013 disk:Inconsistent backing_dev:/dev/ambar-exp/p13 quorum:no
  volume:0 replication:Off peer-disk:DUnknown resync-suspended:no
  received:0 sent:0 out-of-sync:1048508 pending:0 unacked:0
t+38 s [dv-3]
  volume:0 minor:1013 disk:Inconsistent backing_dev:/dev/ambar-exp/p13 quorum:no
  volume:0 replication:Off peer-disk:DUnknown resync-suspended:no
  received:0 sent:0 out-of-sync:1048508 pending:0 unacked:0
t+44 s [dv-3]
  volume:0 minor:1013 disk:Inconsistent backing_dev:/dev/ambar-exp/p13 quorum:no
  volume:0 replication:Off peer-disk:DUnknown resync-suspended:no
  received:0 sent:0 out-of-sync:1048508 pending:0 unacked:0
t+50 s [dv-3]
  volume:0 minor:1013 disk:Inconsistent backing_dev:/dev/ambar-exp/p13 quorum:no
  volume:0 replication:Off peer-disk:DUnknown resync-suspended:no
  received:0 sent:0 out-of-sync:1048508 pending:0 unacked:0
t+57 s [dv-3]
  volume:0 minor:1013 disk:Inconsistent backing_dev:/dev/ambar-exp/p13 quorum:no
  volume:0 replication:Off peer-disk:DUnknown resync-suspended:no
  received:0 sent:0 out-of-sync:1048508 pending:0 unacked:0
t+63 s [dv-3]
  volume:0 minor:1013 disk:Inconsistent backing_dev:/dev/ambar-exp/p13 quorum:no
  volume:0 replication:Off peer-disk:DUnknown resync-suspended:no
  received:0 sent:0 out-of-sync:1048508 pending:0 unacked:0
t+69 s [dv-3]
  volume:0 minor:1013 disk:Inconsistent backing_dev:/dev/ambar-exp/p13 quorum:no
  volume:0 replication:Off peer-disk:DUnknown resync-suspended:no
  received:0 sent:0 out-of-sync:1048508 pending:0 unacked:0
t+75 s [dv-3]
  volume:0 minor:1013 disk:Inconsistent backing_dev:/dev/ambar-exp/p13 quorum:no
  volume:0 replication:Off peer-disk:DUnknown resync-suspended:no
  received:0 sent:0 out-of-sync:1048508 pending:0 unacked:0
t+82 s [dv-3]
  volume:0 minor:1013 disk:Inconsistent backing_dev:/dev/ambar-exp/p13 quorum:no
  volume:0 replication:Off peer-disk:DUnknown resync-suspended:no
  received:0 sent:0 out-of-sync:1048508 pending:0 unacked:0
t+88 s [dv-3]
  volume:0 minor:1013 disk:Inconsistent backing_dev:/dev/ambar-exp/p13 quorum:no
  volume:0 replication:Off peer-disk:DUnknown resync-suspended:no
  received:0 sent:0 out-of-sync:1048508 pending:0 unacked:0
t+94 s [dv-3]
  volume:0 minor:1013 disk:Inconsistent backing_dev:/dev/ambar-exp/p13 quorum:no
  volume:0 replication:Off peer-disk:DUnknown resync-suspended:no
  received:0 sent:0 out-of-sync:1048508 pending:0 unacked:0
t+100 s [dv-3]
  volume:0 minor:1013 disk:Inconsistent backing_dev:/dev/ambar-exp/p13 quorum:no
  volume:0 replication:Off peer-disk:DUnknown resync-suspended:no
  received:0 sent:0 out-of-sync:1048508 pending:0 unacked:0
t+107 s [dv-3]
  volume:0 minor:1013 disk:Inconsistent backing_dev:/dev/ambar-exp/p13 quorum:no
  volume:0 replication:Off peer-disk:DUnknown resync-suspended:no
  received:0 sent:0 out-of-sync:1048508 pending:0 unacked:0
t+113 s [dv-3]
  volume:0 minor:1013 disk:Inconsistent backing_dev:/dev/ambar-exp/p13 quorum:no
  volume:0 replication:Off peer-disk:DUnknown resync-suspended:no
  received:0 sent:0 out-of-sync:1048508 pending:0 unacked:0
t+119 s [dv-3]
  volume:0 minor:1013 disk:Inconsistent backing_dev:/dev/ambar-exp/p13 quorum:no
  volume:0 replication:Off peer-disk:DUnknown resync-suspended:no
  received:0 sent:0 out-of-sync:1048508 pending:0 unacked:0
t+125 s [dv-3]
  volume:0 minor:1013 disk:Inconsistent backing_dev:/dev/ambar-exp/p13 quorum:no
  volume:0 replication:Off peer-disk:DUnknown resync-suspended:no
  received:0 sent:0 out-of-sync:1048508 pending:0 unacked:0
NOT UpToDate on dv-3 after 120 s
P13: dv-3 did not reach UpToDate, no hash comparison
$ [dv-1] drbdsetup status p13 --verbose --statistics
p13 node-id:0 role:Secondary suspended:no force-io-failures:no
    write-ordering:flush
  volume:0 minor:1013 disk:UpToDate backing_dev:/dev/ambar-exp/p13 quorum:yes
      open:no
      size:1048508 read:0 written:65536 al-writes:16 bm-writes:8 upper-pending:0
      lower-pending:0 al-suspended:no blocked:no
  lima-dv-3 node-id:2 connection:StandAlone role:Unknown tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Off peer-disk:DUnknown resync-suspended:no
        received:0 sent:0 out-of-sync:0 pending:0 unacked:0

rc=0
$ [dv-3] drbdsetup status p13 --verbose --statistics
p13 node-id:2 role:Secondary suspended:no force-io-failures:no
    write-ordering:flush
  volume:0 minor:1013 disk:Inconsistent backing_dev:/dev/ambar-exp/p13 quorum:no
      open:no
      size:1048508 read:0 written:0 al-writes:0 bm-writes:0 upper-pending:0
      lower-pending:0 al-suspended:yes blocked:no
  lima-dv-1 node-id:0 connection:Connecting role:Unknown tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Off peer-disk:DUnknown resync-suspended:no
        received:0 sent:0 out-of-sync:1048508 pending:0 unacked:0

rc=0

### P13 run 2: forget-peer p13 1 on dv-1 after the node left
$ [dv-1] drbdadm create-md --force --max-peers=1 p13
initializing bitmap (32 KB) to all zero
initializing activity log
Writing meta data...
New drbd meta data block successfully created.
rc=0
$ [dv-2] drbdadm create-md --force --max-peers=1 p13
initializing bitmap (32 KB) to all zero
initializing activity log
Writing meta data...
New drbd meta data block successfully created.
rc=0
$ [dv-1] drbdadm up p13
rc=0
$ [dv-2] drbdadm up p13
rc=0
wait: dv-1: 1 connections Connected after 2 s at most
wait: dv-2: 1 connections Connected after 2 s at most
$ [dv-1] drbdadm new-current-uuid --clear-bitmap p13/0
rc=0
$ [dv-1] drbdsetup status p13 --verbose
p13 node-id:0 role:Secondary suspended:no force-io-failures:no
  volume:0 minor:1013 disk:UpToDate backing_dev:/dev/ambar-exp/p13 quorum:yes
      open:no blocked:no
  lima-dv-2 node-id:1 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no

rc=0
$ [dv-1] dd if=/dev/urandom of=/dev/drbd1013 bs=1M count=64 oflag=direct status=none
rc=0
$ [dv-1] drbdsetup status p13 --verbose --statistics
p13 node-id:0 role:Secondary suspended:no force-io-failures:no
    write-ordering:flush
  volume:0 minor:1013 disk:UpToDate backing_dev:/dev/ambar-exp/p13 quorum:yes
      open:no
      size:1048508 read:0 written:65536 al-writes:16 bm-writes:8 upper-pending:0
      lower-pending:0 al-suspended:no blocked:no
  lima-dv-2 node-id:1 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
        received:0 sent:65536 out-of-sync:0 pending:0 unacked:0

rc=0

### P13 step 1: dv-2 leaves (down on dv-2, p13.res without dv-2 on dv-1, adjust on dv-1)
@ 20:04:03.270 down on dv-2
$ [dv-2] drbdadm down p13
rc=0
$ [dv-1] drbdadm adjust p13
rc=0
$ [dv-1] drbdsetup status p13 --verbose
p13 node-id:0 role:Secondary suspended:no force-io-failures:no
  volume:0 minor:1013 disk:UpToDate backing_dev:/dev/ambar-exp/p13 quorum:yes
      open:no blocked:no

rc=0
@ 20:04:10.344 forget-peer 1 on dv-1
$ [dv-1] drbdsetup forget-peer p13 1
rc=0
$ [dv-1] drbdsetup status p13 --verbose
p13 node-id:0 role:Secondary suspended:no force-io-failures:no
  volume:0 minor:1013 disk:UpToDate backing_dev:/dev/ambar-exp/p13 quorum:yes
      open:no blocked:no

rc=0

### P13 step 2: dv-3 (node-id 2) joins with a copy
$ [dv-3] drbdadm create-md --force --max-peers=1 p13
initializing bitmap (32 KB) to all zero
initializing activity log
Writing meta data...
New drbd meta data block successfully created.
rc=0
$ [dv-3] drbdadm up p13
rc=0
@ 20:04:16.655 adjust on dv-1
$ [dv-1] drbdadm adjust p13
rc=0
$ [dv-1] drbdsetup status p13 --verbose --statistics
p13 node-id:0 role:Secondary suspended:no force-io-failures:no
    write-ordering:flush
  volume:0 minor:1013 disk:UpToDate backing_dev:/dev/ambar-exp/p13 quorum:yes
      open:no
      size:1048508 read:1048508 written:65536 al-writes:16 bm-writes:29 upper-pending:0
      lower-pending:0 al-suspended:no blocked:no
  lima-dv-3 node-id:2 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
        received:0 sent:1048508 out-of-sync:0 pending:0 unacked:0

rc=0
$ [dv-3] drbdsetup status p13 --verbose --statistics
p13 node-id:2 role:Secondary suspended:no force-io-failures:no
    write-ordering:flush
  volume:0 minor:1013 disk:UpToDate backing_dev:/dev/ambar-exp/p13 quorum:yes
      open:no
      size:1048508 read:0 written:1048508 al-writes:0 bm-writes:21 upper-pending:0
      lower-pending:0 al-suspended:no blocked:no
  lima-dv-1 node-id:0 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
        received:1048508 sent:0 out-of-sync:0 pending:0 unacked:0

rc=0
--- dmesg -T | grep 'drbd p13' | tail -n 80 [dv-1]
[Tue Nov 17 20:03:47 2026] drbd p13/0 drbd1013: resync bitmap: bits=262127 bits_4k=262127 words=4096 pages=8
[Tue Nov 17 20:03:47 2026] drbd p13/0 drbd1013: size = 1024 MB (1048508 KB)
[Tue Nov 17 20:03:47 2026] drbd p13/0 drbd1013: Suspended AL updates
[Tue Nov 17 20:03:47 2026] drbd p13/0 drbd1013: disk( Attaching -> Inconsistent ) [attach]
[Tue Nov 17 20:03:47 2026] drbd p13/0 drbd1013: attached to current UUID: 0000000000000004
[Tue Nov 17 20:03:47 2026] drbd p13 lima-dv-2: conn( StandAlone -> Unconnected ) [connect]
[Tue Nov 17 20:03:47 2026] drbd p13 lima-dv-2: Starting receiver thread (peer-node-id 1)
[Tue Nov 17 20:03:47 2026] drbd p13 lima-dv-2: conn( Unconnected -> Connecting ) [connecting]
[Tue Nov 17 20:03:48 2026] drbd p13 lima-dv-2: Handshake to peer 1 successful: Agreed network protocol version 125
[Tue Nov 17 20:03:48 2026] drbd p13 lima-dv-2: Feature flags enabled on protocol level: 0x7ff TRIM THIN_RESYNC WRITE_SAME WRITE_ZEROES RESYNC_DAGTAG WRITE_POSTPONE
[Tue Nov 17 20:03:48 2026] drbd p13: Preparing cluster-wide state change 1613293733: 0->1 role( Secondary ) conn( Connected )
[Tue Nov 17 20:03:48 2026] drbd p13/0 drbd1013 lima-dv-2: drbd_sync_handshake:
[Tue Nov 17 20:03:48 2026] drbd p13/0 drbd1013 lima-dv-2: self 0000000000000004:0000000000000000:0000000000000000:0000000000000000 bits:262127 flags:24
[Tue Nov 17 20:03:48 2026] drbd p13/0 drbd1013 lima-dv-2: peer 0000000000000004:0000000000000000:0000000000000000:0000000000000000 bits:262127 flags:24
[Tue Nov 17 20:03:48 2026] drbd p13/0 drbd1013 lima-dv-2: uuid_compare()=no-sync by rule=just-created-both
[Tue Nov 17 20:03:48 2026] drbd p13: State change 1613293733: primary_nodes=0, weak_nodes=0
[Tue Nov 17 20:03:48 2026] drbd p13: Committing cluster-wide state change 1613293733 (16ms)
[Tue Nov 17 20:03:48 2026] drbd p13 lima-dv-2: conn( Connecting -> Connected ) peer( Unknown -> Secondary ) [connected]
[Tue Nov 17 20:03:48 2026] drbd p13/0 drbd1013: quorum( no -> yes ) [connected]
[Tue Nov 17 20:03:48 2026] drbd p13/0 drbd1013 lima-dv-2: pdsk( DUnknown -> Inconsistent ) repl( Off -> Established ) replication( yes -> no ) peer_replication( yes -> no ) [connected]
[Tue Nov 17 20:03:48 2026] drbd p13/0 drbd1013: Resumed AL updates
[Tue Nov 17 20:03:54 2026] drbd p13/0 drbd1013 lima-dv-2: Preparing to skip initial sync
[Tue Nov 17 20:03:54 2026] drbd p13/0 drbd1013: Bitmap UUID for initial resync: 4B66B2FB4DFBD70E
[Tue Nov 17 20:03:54 2026] drbd p13/0 drbd1013: new current UUID: 839B35B91FAFEA94 weak: FFFFFFFFFFFFFFFD
[Tue Nov 17 20:03:54 2026] drbd p13/0 drbd1013 lima-dv-2: cleared bitmap UUID 839B35B91FAFEA94:0000000000000000:0000000000000000:0000000000000000
[Tue Nov 17 20:03:54 2026] drbd p13/0 drbd1013: disk( Inconsistent -> UpToDate ) [new-c-uuid]
[Tue Nov 17 20:03:54 2026] drbd p13/0 drbd1013 lima-dv-2: pdsk( Inconsistent -> UpToDate ) replication( no -> yes ) [new-c-uuid]
[Tue Nov 17 20:03:54 2026] drbd p13/0 drbd1013: persisting effective size = 1024 MB (1048508 KB)
[Tue Nov 17 20:03:59 2026] drbd p13: Preparing cluster-wide state change 1491886064: 0->all role( Primary )
[Tue Nov 17 20:03:59 2026] drbd p13: State change 1491886064: primary_nodes=1, weak_nodes=FFFFFFFFFFFFFFFC
[Tue Nov 17 20:03:59 2026] drbd p13: Committing cluster-wide state change 1491886064 (0ms)
[Tue Nov 17 20:03:59 2026] drbd p13: role( Secondary -> Primary ) [dd:322680 auto-promote]
[Tue Nov 17 20:03:59 2026] drbd p13/0 drbd1013: gen-obligation: NONE -> ARMED(promoted)
[Tue Nov 17 20:03:59 2026] drbd p13/0 drbd1013: gen-obligation: ARMED(promoted) -> MINTING(promoted)
[Tue Nov 17 20:03:59 2026] drbd p13/0 drbd1013: gen-obligation: MINTING(promoted) -> DISCHARGED(promoted)
[Tue Nov 17 20:03:59 2026] drbd p13: Preparing cluster-wide state change 2821345583: 0->all role( Secondary )
[Tue Nov 17 20:03:59 2026] drbd p13: State change 2821345583: primary_nodes=0, weak_nodes=0
[Tue Nov 17 20:03:59 2026] drbd p13: Committing cluster-wide state change 2821345583 (0ms)
[Tue Nov 17 20:03:59 2026] drbd p13: role( Primary -> Secondary ) [dd:322680 auto-demote]
[Tue Nov 17 20:04:02 2026] drbd p13: Preparing remote state change 2931775133: 1->0 conn( Disconnecting )
[Tue Nov 17 20:04:02 2026] drbd p13 lima-dv-2: Committing remote state change 2931775133 (primary_nodes=0)
[Tue Nov 17 20:04:02 2026] drbd p13 lima-dv-2: conn( Connected -> TearDown ) peer( Secondary -> Unknown ) [remote]
[Tue Nov 17 20:04:02 2026] drbd p13/0 drbd1013 lima-dv-2: pdsk( UpToDate -> DUnknown ) repl( Established -> Off ) [remote]
[Tue Nov 17 20:04:02 2026] drbd p13 lima-dv-2: Terminating sender thread
[Tue Nov 17 20:04:02 2026] drbd p13 lima-dv-2: Starting sender thread (peer-node-id 1)
[Tue Nov 17 20:04:02 2026] drbd p13 lima-dv-2: Connection closed
[Tue Nov 17 20:04:02 2026] drbd p13 lima-dv-2: conn( TearDown -> Unconnected ) [disconnected]
[Tue Nov 17 20:04:02 2026] drbd p13 lima-dv-2: Restarting receiver thread
[Tue Nov 17 20:04:02 2026] drbd p13 lima-dv-2: conn( Unconnected -> Connecting ) [connecting]
[Tue Nov 17 20:04:03 2026] drbd p13 lima-dv-2: conn( Connecting -> Disconnecting ) [del-peer]
[Tue Nov 17 20:04:03 2026] drbd p13 lima-dv-2: Terminating sender thread
[Tue Nov 17 20:04:03 2026] drbd p13 lima-dv-2: Starting sender thread (peer-node-id 1)
[Tue Nov 17 20:04:03 2026] drbd p13 lima-dv-2: Connection closed
[Tue Nov 17 20:04:03 2026] drbd p13 lima-dv-2: conn( Disconnecting -> StandAlone ) [disconnected]
[Tue Nov 17 20:04:03 2026] drbd p13 lima-dv-2: Terminating receiver thread
[Tue Nov 17 20:04:03 2026] drbd p13 lima-dv-2: Terminating sender thread
[Tue Nov 17 20:04:15 2026] drbd p13 lima-dv-3: Starting sender thread (peer-node-id 2)
[Tue Nov 17 20:04:15 2026] drbd p13 lima-dv-3: conn( StandAlone -> Unconnected ) [connect]
[Tue Nov 17 20:04:15 2026] drbd p13 lima-dv-3: Starting receiver thread (peer-node-id 2)
[Tue Nov 17 20:04:15 2026] drbd p13 lima-dv-3: conn( Unconnected -> Connecting ) [connecting]
[Tue Nov 17 20:04:15 2026] drbd p13 lima-dv-3: Handshake to peer 2 successful: Agreed network protocol version 125
[Tue Nov 17 20:04:15 2026] drbd p13 lima-dv-3: Feature flags enabled on protocol level: 0x7ff TRIM THIN_RESYNC WRITE_SAME WRITE_ZEROES RESYNC_DAGTAG WRITE_POSTPONE
[Tue Nov 17 20:04:15 2026] drbd p13: Preparing cluster-wide state change 2702460548: 0->2 role( Secondary ) conn( Connected )
[Tue Nov 17 20:04:15 2026] drbd p13/0 drbd1013 lima-dv-3: drbd_sync_handshake:
[Tue Nov 17 20:04:15 2026] drbd p13/0 drbd1013 lima-dv-3: self 839B35B91FAFEA94:4B66B2FB4DFBD70E:0000000000000000:0000000000000000 bits:262127 flags:20
[Tue Nov 17 20:04:15 2026] drbd p13/0 drbd1013 lima-dv-3: peer 0000000000000004:0000000000000000:0000000000000000:0000000000000000 bits:262127 flags:24
[Tue Nov 17 20:04:15 2026] drbd p13/0 drbd1013 lima-dv-3: uuid_compare()=source-set-bitmap by rule=just-created-peer
[Tue Nov 17 20:04:15 2026] drbd p13/0 drbd1013 lima-dv-3: Setting and writing one bitmap slot, after drbd_sync_handshake
[Tue Nov 17 20:04:15 2026] drbd p13: State change 2702460548: primary_nodes=0, weak_nodes=0
[Tue Nov 17 20:04:15 2026] drbd p13: Committing cluster-wide state change 2702460548 (20ms)
[Tue Nov 17 20:04:15 2026] drbd p13 lima-dv-3: conn( Connecting -> Connected ) peer( Unknown -> Secondary ) [connected]
[Tue Nov 17 20:04:15 2026] drbd p13/0 drbd1013 lima-dv-3: pdsk( DUnknown -> Inconsistent ) repl( Off -> WFBitMapS ) replication( yes -> no ) [connected]
[Tue Nov 17 20:04:15 2026] drbd p13/0 drbd1013 lima-dv-3: send bitmap stats [Bytes(packets)]: plain 0(0), RLE 21(1), total 21; compression: 100.0%
[Tue Nov 17 20:04:15 2026] drbd p13/0 drbd1013 lima-dv-3: receive bitmap stats [Bytes(packets)]: plain 0(0), RLE 21(1), total 21; compression: 100.0%
[Tue Nov 17 20:04:15 2026] drbd p13/0 drbd1013 lima-dv-3: repl( WFBitMapS -> SyncSource ) [receive-bitmap]
[Tue Nov 17 20:04:15 2026] drbd p13/0 drbd1013 lima-dv-3: Began resync as SyncSource (will sync 1048508 KB [262127 bits set]).
[Tue Nov 17 20:04:27 2026] drbd p13/0 drbd1013 lima-dv-3: replication( no -> yes ) [enable-replication]
[Tue Nov 17 20:04:27 2026] drbd p13/0 drbd1013 lima-dv-3: updated UUIDs 839B35B91FAFEA94:0000000000000000:4B66B2FB4DFBD70E:0000000000000000
[Tue Nov 17 20:04:27 2026] drbd p13/0 drbd1013 lima-dv-3: Resync done (total 11 sec; paused 0 sec; 95316 K/sec)
[Tue Nov 17 20:04:27 2026] drbd p13/0 drbd1013 lima-dv-3: pdsk( Inconsistent -> UpToDate ) repl( SyncSource -> Established ) [resync-finished]
--- dmesg -T | grep 'drbd p13' | tail -n 80 [dv-3]
[Tue Nov 17 20:00:19 2026] drbd p13: Starting worker thread (node-id 2)
[Tue Nov 17 20:00:19 2026] drbd p13 lima-dv-1: Starting sender thread (peer-node-id 0)
[Tue Nov 17 20:00:19 2026] drbd p13/0 drbd1013: meta-data IO uses: blk-bio
[Tue Nov 17 20:00:19 2026] drbd p13/0 drbd1013: disk( Diskless -> Attaching ) [attach]
[Tue Nov 17 20:00:19 2026] drbd p13/0 drbd1013: Maximum number of peer devices = 1
[Tue Nov 17 20:00:19 2026] drbd p13: Method to ensure write ordering: flush
[Tue Nov 17 20:00:19 2026] drbd p13/0 drbd1013: Allocating 32 KiB for new bitmap
[Tue Nov 17 20:00:19 2026] drbd p13/0 drbd1013: resync bitmap: bits=262127 bits_4k=262127 words=4096 pages=8
[Tue Nov 17 20:00:19 2026] drbd p13/0 drbd1013: size = 1024 MB (1048508 KB)
[Tue Nov 17 20:00:19 2026] drbd p13/0 drbd1013: Suspended AL updates
[Tue Nov 17 20:00:19 2026] drbd p13/0 drbd1013: disk( Attaching -> Inconsistent ) [attach]
[Tue Nov 17 20:00:19 2026] drbd p13/0 drbd1013: attached to current UUID: 0000000000000004
[Tue Nov 17 20:00:19 2026] drbd p13 lima-dv-1: conn( StandAlone -> Unconnected ) [connect]
[Tue Nov 17 20:00:19 2026] drbd p13 lima-dv-1: Starting receiver thread (peer-node-id 0)
[Tue Nov 17 20:00:19 2026] drbd p13 lima-dv-1: conn( Unconnected -> Connecting ) [connecting]
[Tue Nov 17 20:03:25 2026] drbd p13 lima-dv-1: conn( Connecting -> Disconnecting ) [down]
[Tue Nov 17 20:03:25 2026] drbd p13 lima-dv-1: Terminating sender thread
[Tue Nov 17 20:03:25 2026] drbd p13 lima-dv-1: Starting sender thread (peer-node-id 0)
[Tue Nov 17 20:03:25 2026] drbd p13 lima-dv-1: Connection closed
[Tue Nov 17 20:03:25 2026] drbd p13 lima-dv-1: conn( Disconnecting -> StandAlone ) [disconnected]
[Tue Nov 17 20:03:25 2026] drbd p13 lima-dv-1: Terminating receiver thread
[Tue Nov 17 20:03:25 2026] drbd p13 lima-dv-1: Terminating sender thread
[Tue Nov 17 20:03:25 2026] drbd p13/0 drbd1013: disk( Inconsistent -> Detaching ) quorum( no -> yes ) [down]
[Tue Nov 17 20:03:25 2026] drbd p13/0 drbd1013: disk( Detaching -> Diskless ) quorum( yes -> no ) [go-diskless]
[Tue Nov 17 20:03:25 2026] drbd p13/0 drbd1013: Freeing bitmap of size 32 KiB
[Tue Nov 17 20:04:13 2026] drbd p13: Starting worker thread (node-id 2)
[Tue Nov 17 20:04:13 2026] drbd p13 lima-dv-1: Starting sender thread (peer-node-id 0)
[Tue Nov 17 20:04:13 2026] drbd p13/0 drbd1013: meta-data IO uses: blk-bio
[Tue Nov 17 20:04:13 2026] drbd p13/0 drbd1013: disk( Diskless -> Attaching ) [attach]
[Tue Nov 17 20:04:13 2026] drbd p13/0 drbd1013: Maximum number of peer devices = 1
[Tue Nov 17 20:04:13 2026] drbd p13: Method to ensure write ordering: flush
[Tue Nov 17 20:04:13 2026] drbd p13/0 drbd1013: Allocating 32 KiB for new bitmap
[Tue Nov 17 20:04:13 2026] drbd p13/0 drbd1013: resync bitmap: bits=262127 bits_4k=262127 words=4096 pages=8
[Tue Nov 17 20:04:13 2026] drbd p13/0 drbd1013: size = 1024 MB (1048508 KB)
[Tue Nov 17 20:04:13 2026] drbd p13/0 drbd1013: Suspended AL updates
[Tue Nov 17 20:04:13 2026] drbd p13/0 drbd1013: disk( Attaching -> Inconsistent ) [attach]
[Tue Nov 17 20:04:13 2026] drbd p13/0 drbd1013: attached to current UUID: 0000000000000004
[Tue Nov 17 20:04:13 2026] drbd p13 lima-dv-1: conn( StandAlone -> Unconnected ) [connect]
[Tue Nov 17 20:04:13 2026] drbd p13 lima-dv-1: Starting receiver thread (peer-node-id 0)
[Tue Nov 17 20:04:13 2026] drbd p13 lima-dv-1: conn( Unconnected -> Connecting ) [connecting]
[Tue Nov 17 20:04:16 2026] drbd p13 lima-dv-1: Handshake to peer 0 successful: Agreed network protocol version 125
[Tue Nov 17 20:04:16 2026] drbd p13 lima-dv-1: Feature flags enabled on protocol level: 0x7ff TRIM THIN_RESYNC WRITE_SAME WRITE_ZEROES RESYNC_DAGTAG WRITE_POSTPONE
[Tue Nov 17 20:04:16 2026] drbd p13: Preparing remote state change 2702460548: 0->2 role( Secondary ) conn( Connected )
[Tue Nov 17 20:04:16 2026] drbd p13/0 drbd1013 lima-dv-1: drbd_sync_handshake:
[Tue Nov 17 20:04:16 2026] drbd p13/0 drbd1013 lima-dv-1: self 0000000000000004:0000000000000000:0000000000000000:0000000000000000 bits:262127 flags:24
[Tue Nov 17 20:04:16 2026] drbd p13/0 drbd1013 lima-dv-1: peer 839B35B91FAFEA94:4B66B2FB4DFBD70E:0000000000000000:0000000000000000 bits:262127 flags:20
[Tue Nov 17 20:04:16 2026] drbd p13/0 drbd1013 lima-dv-1: uuid_compare()=target-set-bitmap by rule=just-created-self
[Tue Nov 17 20:04:16 2026] drbd p13/0 drbd1013 lima-dv-1: Setting and writing the whole bitmap, fresh node
[Tue Nov 17 20:04:16 2026] drbd p13 lima-dv-1: Committing remote state change 2702460548 (primary_nodes=0)
[Tue Nov 17 20:04:16 2026] drbd p13 lima-dv-1: conn( Connecting -> Connected ) peer( Unknown -> Secondary ) [remote]
[Tue Nov 17 20:04:16 2026] drbd p13/0 drbd1013: quorum( no -> yes ) [remote]
[Tue Nov 17 20:04:16 2026] drbd p13/0 drbd1013 lima-dv-1: pdsk( DUnknown -> UpToDate ) repl( Off -> WFBitMapT ) peer_replication( yes -> no ) [remote]
[Tue Nov 17 20:04:16 2026] drbd p13/0 drbd1013: Resumed AL updates
[Tue Nov 17 20:04:16 2026] drbd p13/0 drbd1013: Setting exposed data uuid: 839B35B91FAFEA94
[Tue Nov 17 20:04:16 2026] drbd p13/0 drbd1013: persisting effective size = 1024 MB (1048508 KB)
[Tue Nov 17 20:04:16 2026] drbd p13/0 drbd1013 lima-dv-1: receive bitmap stats [Bytes(packets)]: plain 0(0), RLE 21(1), total 21; compression: 100.0%
[Tue Nov 17 20:04:16 2026] drbd p13/0 drbd1013 lima-dv-1: send bitmap stats [Bytes(packets)]: plain 0(0), RLE 21(1), total 21; compression: 100.0%
[Tue Nov 17 20:04:16 2026] drbd p13/0 drbd1013 lima-dv-1: setting UUIDs to 4B66B2FB4DFBD70E:0000000000000000:0000000000000000:0000000000000000
[Tue Nov 17 20:04:16 2026] drbd p13/0 drbd1013 lima-dv-1: repl( WFBitMapT -> SyncTarget ) [receive-bitmap]
[Tue Nov 17 20:04:16 2026] drbd p13/0 drbd1013: Setting exposed data uuid: 839B35B91FAFEA94
[Tue Nov 17 20:04:16 2026] drbd p13/0 drbd1013 lima-dv-1: Began resync as SyncTarget (will sync 1048508 KB [262127 bits set]).
[Tue Nov 17 20:04:27 2026] drbd p13/0 drbd1013 lima-dv-1: peer_replication( no -> yes ) [enable-peer-replication]
[Tue Nov 17 20:04:27 2026] drbd p13/0 drbd1013 lima-dv-1: Resync done (total 11 sec; paused 0 sec; 95316 K/sec)
[Tue Nov 17 20:04:27 2026] drbd p13/0 drbd1013 lima-dv-1: updated UUIDs 839B35B91FAFEA94:0000000000000000:0000000000000000:0000000000000000
[Tue Nov 17 20:04:27 2026] drbd p13/0 drbd1013: disk( Inconsistent -> UpToDate ) [resync-finished]
[Tue Nov 17 20:04:27 2026] drbd p13/0 drbd1013 lima-dv-1: repl( SyncTarget -> Established ) [resync-finished]

### P13 step 4: UpToDate on dv-3 (up to 120 s more), then the hashes
t+1 s [dv-3]
  volume:0 minor:1013 disk:UpToDate backing_dev:/dev/ambar-exp/p13 quorum:yes
  volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
  received:1048508 sent:0 out-of-sync:0 pending:0 unacked:0
UpToDate on dv-3: t+1 s after the mark (poll step 5 s plus the call latency)
$ [dv-1] dd if=/dev/drbd1013 bs=1M count=64 iflag=direct status=none | sha256sum
abc698ca27e9875612391be953337219270cbf8090a14a76a64e5ad7caca6770  -
rc=0
$ [dv-3] drbdadm primary p13
rc=0
$ [dv-3] dd if=/dev/drbd1013 bs=1M count=64 iflag=direct status=none | sha256sum
abc698ca27e9875612391be953337219270cbf8090a14a76a64e5ad7caca6770  -
rc=0
$ [dv-3] drbdadm secondary p13
rc=0
$ [dv-1] drbdsetup status p13 --verbose --statistics
p13 node-id:0 role:Secondary suspended:no force-io-failures:no
    write-ordering:flush
  volume:0 minor:1013 disk:UpToDate backing_dev:/dev/ambar-exp/p13 quorum:yes
      open:no
      size:1048508 read:1114044 written:65536 al-writes:16 bm-writes:29 upper-pending:0
      lower-pending:0 al-suspended:no blocked:no
  lima-dv-3 node-id:2 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
        received:0 sent:1048508 out-of-sync:0 pending:0 unacked:0

rc=0
$ [dv-3] drbdsetup status p13 --verbose --statistics
p13 node-id:2 role:Secondary suspended:no force-io-failures:no
    write-ordering:flush
  volume:0 minor:1013 disk:UpToDate backing_dev:/dev/ambar-exp/p13 quorum:yes
      open:no
      size:1048508 read:65536 written:1048508 al-writes:0 bm-writes:21 upper-pending:0
      lower-pending:0 al-suspended:no blocked:no
  lima-dv-1 node-id:0 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
        received:1048508 sent:0 out-of-sync:0 pending:0 unacked:0

rc=0
exit=0
```

### P14. Кворум при синхронизирующейся копии Inconsistent

Ресурс `p14`: том 4 ГиБ (2 ГиБ данных, непрерывная запись), port 7014, minor 1014; прогоны со свидетелем (`dv-4`) и без свидетеля.

```bash
stand/spike/pre-2b.sh p14
```

```text
### P14 variant with a witness, run 1
$ [dv-1] drbdadm create-md --force --max-peers=7 p14
initializing bitmap (896 KB) to all zero
initializing activity log
Writing meta data...
New drbd meta data block successfully created.
rc=0
$ [dv-2] drbdadm create-md --force --max-peers=7 p14
initializing bitmap (896 KB) to all zero
initializing activity log
Writing meta data...
New drbd meta data block successfully created.
rc=0
$ [dv-1] drbdadm up p14
rc=0
$ [dv-2] drbdadm up p14
rc=0
$ [dv-4] drbdadm up p14
rc=0
wait: dv-1: 2 connections Connected after 2 s at most
wait: dv-2: 2 connections Connected after 2 s at most
wait: dv-4: 2 connections Connected after 2 s at most
$ [dv-1] drbdadm new-current-uuid --clear-bitmap p14/0
rc=0
$ [dv-1] drbdsetup status p14 --verbose
p14 node-id:0 role:Secondary suspended:no force-io-failures:no
  volume:0 minor:1014 disk:UpToDate backing_dev:/dev/ambar-exp/p14 quorum:yes
      open:no blocked:no
  lima-dv-2 node-id:1 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
  lima-dv-4 node-id:3 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:Diskless peer-client:yes
        resync-suspended:no

rc=0
$ [dv-1] dd if=/dev/urandom of=/dev/drbd1014 bs=1M count=2048 oflag=direct status=none
rc=0
$ [dv-1] mkfs.ext4 -q -F -E nodiscard /dev/drbd1014
rc=0
20:06:51.390
2373
started
free space on dv-1 after the 2 GiB write: /dev/vda1        20G  4.4G   15G  24% /

### P14: dv-3 joins with a copy
$ [dv-3] drbdadm create-md --force --max-peers=7 p14
initializing bitmap (896 KB) to all zero
initializing activity log
Writing meta data...
New drbd meta data block successfully created.
rc=0
$ [dv-3] drbdadm up p14
rc=0
@ 20:06:56.801 up on dv-3 (UP_T0)
$ [dv-1] drbdadm adjust p14
rc=0
$ [dv-2] drbdadm adjust p14
rc=0
$ [dv-4] drbdadm adjust p14
rc=0

### P14: wait for dv-3 Inconsistent, then down on dv-2
p14 node-id:0 role:Primary suspended:no force-io-failures:no
    write-ordering:flush
  volume:0 minor:1014 disk:UpToDate backing_dev:/dev/ambar-exp/p14 quorum:yes
      open:yes
      size:4193372 read:51245 written:2339144 al-writes:522 bm-writes:455
      upper-pending:1 lower-pending:2 al-suspended:no blocked:no
  lima-dv-2 node-id:1 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:24 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
        received:0 sent:2207980 out-of-sync:0 pending:1 unacked:0
  lima-dv-3 node-id:2 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:SyncSource peer-disk:Inconsistent done:1.18
        resync-suspended:no
        received:0 sent:49440 out-of-sync:4143932 pending:0 unacked:1 dbdt1:19.59 eta:207
  lima-dv-4 node-id:3 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:Diskless peer-client:yes
        resync-suspended:no
        received:0 sent:0 out-of-sync:0 pending:0 unacked:0
@ 20:07:03.045 dv-3 seen Inconsistent (t+6 s after up); down on dv-2
$ [dv-2] drbdadm down p14
rc=0
@ 20:07:06.085 down on dv-2 returned
$ [dv-1] drbdsetup status p14 --verbose --statistics
p14 node-id:0 role:Primary suspended:no force-io-failures:no
    write-ordering:flush
  volume:0 minor:1014 disk:UpToDate backing_dev:/dev/ambar-exp/p14 quorum:yes
      open:yes
      size:4193372 read:53357 written:2399940 al-writes:522 bm-writes:458
      upper-pending:0 lower-pending:1 al-suspended:no blocked:no
  lima-dv-2 node-id:1 connection:Connecting role:Unknown tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Off peer-disk:Outdated resync-suspended:no
        received:0 sent:0 out-of-sync:39272 pending:0 unacked:0
  lima-dv-3 node-id:2 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:SyncSource peer-disk:Inconsistent done:1.23
        resync-suspended:no
        received:0 sent:51552 out-of-sync:4141820 pending:0 unacked:1 dbdt1:0.43 eta:9380
  lima-dv-4 node-id:3 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:Diskless peer-client:yes
        resync-suspended:no
        received:0 sent:0 out-of-sync:0 pending:0 unacked:0

rc=0
@ 20:07:27.307 20 s after down on dv-2
$ [dv-1] drbdsetup status p14 --verbose --statistics
p14 node-id:0 role:Primary suspended:no force-io-failures:no
    write-ordering:flush
  volume:0 minor:1014 disk:UpToDate backing_dev:/dev/ambar-exp/p14 quorum:yes
      open:yes
      size:4193372 read:108301 written:2762356 al-writes:522 bm-writes:471
      upper-pending:0 lower-pending:1 al-suspended:no blocked:no
  lima-dv-2 node-id:1 connection:Connecting role:Unknown tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Off peer-disk:Outdated resync-suspended:no
        received:0 sent:0 out-of-sync:65700 pending:0 unacked:0
  lima-dv-3 node-id:2 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:2048
    volume:0 replication:SyncSource peer-disk:Inconsistent done:2.52
        resync-suspended:no
        received:0 sent:106496 out-of-sync:4087912 pending:1 unacked:1 dbdt1:2.67
        eta:1494
  lima-dv-4 node-id:3 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:Diskless peer-client:yes
        resync-suspended:no
        received:0 sent:0 out-of-sync:0 pending:0 unacked:0

rc=0
$ [dv-3] drbdsetup status p14 --verbose --statistics
p14 node-id:2 role:Secondary suspended:no force-io-failures:no
    write-ordering:flush
  volume:0 minor:1014 disk:Inconsistent backing_dev:/dev/ambar-exp/p14 quorum:yes
      open:no
      size:4193372 read:0 written:115200 al-writes:0 bm-writes:248 upper-pending:0
      lower-pending:0 al-suspended:no blocked:no
  lima-dv-1 node-id:0 connection:Connected role:Primary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:SyncTarget peer-disk:UpToDate done:2.75 resync-suspended:no
        received:115200 sent:0 out-of-sync:4078184 pending:9 unacked:0 dbdt1:4.17 eta:954
  lima-dv-2 node-id:1 connection:Connecting role:Unknown tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Off peer-disk:Outdated resync-suspended:dependency
        received:0 sent:0 out-of-sync:4185180 pending:0 unacked:0
  lima-dv-4 node-id:3 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:Diskless peer-client:yes
        resync-suspended:dependency
        received:0 sent:0 out-of-sync:0 pending:0 unacked:0

rc=0

### P14: dv-2 returns
$ [dv-2] drbdadm up p14
rc=0
@ 20:07:30.554 up on dv-2
wait: dv-2 UpToDate after 112 s at most
wait: dv-3 UpToDate after 254 s at most
@ 20:16:48.830 all copies UpToDate
$ [dv-1] drbdsetup status p14 --verbose --statistics
p14 node-id:0 role:Primary suspended:no force-io-failures:no
    write-ordering:flush
  volume:0 minor:1014 disk:UpToDate backing_dev:/dev/ambar-exp/p14 quorum:yes
      open:yes
      size:4193372 read:4362873 written:7461660 al-writes:522 bm-writes:1004
      upper-pending:1 lower-pending:1 al-suspended:no blocked:no
  lima-dv-2 node-id:1 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:24 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
        received:0 sent:4632000 out-of-sync:0 pending:1 unacked:0
  lima-dv-3 node-id:2 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:24 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
        received:0 sent:4311988 out-of-sync:0 pending:1 unacked:0
  lima-dv-4 node-id:3 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:Diskless peer-client:yes
        resync-suspended:no
        received:0 sent:0 out-of-sync:0 pending:0 unacked:0

rc=0
$ [dv-2] drbdsetup status p14 --verbose --statistics
p14 node-id:1 role:Secondary suspended:no force-io-failures:no
    write-ordering:flush
  volume:0 minor:1014 disk:UpToDate backing_dev:/dev/ambar-exp/p14 quorum:yes
      open:no
      size:4193372 read:0 written:4634052 al-writes:18 bm-writes:392 upper-pending:0
      lower-pending:2 al-suspended:no blocked:no
  lima-dv-1 node-id:0 connection:Connected role:Primary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
        received:4634060 sent:0 out-of-sync:0 pending:0 unacked:0
  lima-dv-3 node-id:2 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
        received:0 sent:0 out-of-sync:111228 pending:0 unacked:0
  lima-dv-4 node-id:3 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:Diskless peer-client:yes
        resync-suspended:no
        received:0 sent:0 out-of-sync:0 pending:0 unacked:0

rc=0
$ [dv-3] drbdsetup status p14 --verbose --statistics
p14 node-id:2 role:Secondary suspended:no force-io-failures:no
    write-ordering:flush
  volume:0 minor:1014 disk:UpToDate backing_dev:/dev/ambar-exp/p14 quorum:yes
      open:no
      size:4193372 read:0 written:4316064 al-writes:7 bm-writes:918 upper-pending:0
      lower-pending:1 al-suspended:no blocked:no
  lima-dv-1 node-id:0 connection:Connected role:Primary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
        received:4316068 sent:0 out-of-sync:0 pending:0 unacked:1
  lima-dv-2 node-id:1 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
        received:0 sent:0 out-of-sync:110580 pending:0 unacked:0
  lima-dv-4 node-id:3 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:Diskless peer-client:yes
        resync-suspended:no
        received:0 sent:0 out-of-sync:0 pending:0 unacked:0

rc=0
$ [dv-4] drbdsetup status p14 --verbose --statistics
p14 node-id:3 role:Secondary suspended:no force-io-failures:no
    write-ordering:none
  volume:0 minor:1014 disk:Diskless client:yes backing_dev:none quorum:yes open:no
      size:4193372 read:0 written:0 al-writes:0 bm-writes:0 upper-pending:0
      lower-pending:0 al-suspended:no blocked:no
  lima-dv-1 node-id:0 connection:Connected role:Primary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
        received:0 sent:0 out-of-sync:0 pending:0 unacked:0
  lima-dv-2 node-id:1 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
        received:0 sent:0 out-of-sync:0 pending:0 unacked:0
  lima-dv-3 node-id:2 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
        received:0 sent:0 out-of-sync:0 pending:0 unacked:0

rc=0
@ 20:16:56.627 stop the writer
acked=326613 on-volume=326614
--- probe.sh log (stdout and stderr)
--- watch.sh log
samples=1196 probe=alive:R=90 probe=alive:S=1106 longest-gap-without-new-acked=0.7s (from 20:13:37.085)
--- quorum log of dv-1 (runs of equal values: first - last  value  samples)
20:06:52.942 - 20:16:57.241  "quorum": true  x602
--- dmesg -T | grep 'drbd p14' | tail -n 80 [dv-1]
[Tue Nov 17 20:06:44 2026] drbd p14/0 drbd1014: gen-obligation: DISCHARGED(promoted) -> ARMED(promoted)
[Tue Nov 17 20:06:44 2026] drbd p14/0 drbd1014: gen-obligation: ARMED(promoted) -> MINTING(promoted)
[Tue Nov 17 20:06:44 2026] drbd p14/0 drbd1014: gen-obligation: MINTING(promoted) -> DISCHARGED(promoted)
[Tue Nov 17 20:06:57 2026] drbd p14 lima-dv-3: Starting sender thread (peer-node-id 2)
[Tue Nov 17 20:06:57 2026] drbd p14 lima-dv-3: conn( StandAlone -> Unconnected ) [connect]
[Tue Nov 17 20:06:57 2026] drbd p14 lima-dv-3: Starting receiver thread (peer-node-id 2)
[Tue Nov 17 20:06:57 2026] drbd p14 lima-dv-3: conn( Unconnected -> Connecting ) [connecting]
[Tue Nov 17 20:06:58 2026] drbd p14 lima-dv-3: Handshake to peer 2 successful: Agreed network protocol version 125
[Tue Nov 17 20:06:58 2026] drbd p14 lima-dv-3: Feature flags enabled on protocol level: 0x7ff TRIM THIN_RESYNC WRITE_SAME WRITE_ZEROES RESYNC_DAGTAG WRITE_POSTPONE
[Tue Nov 17 20:06:58 2026] drbd p14: Preparing cluster-wide state change 2401650196: 0->2 role( Primary ) conn( Connected )
[Tue Nov 17 20:06:58 2026] drbd p14/0 drbd1014 lima-dv-3: drbd_sync_handshake:
[Tue Nov 17 20:06:58 2026] drbd p14/0 drbd1014 lima-dv-3: self 23C2AA16826F4B7E:C65DF829CB93522B:0000000000000000:0000000000000000 bits:532511 flags:20
[Tue Nov 17 20:06:58 2026] drbd p14/0 drbd1014 lima-dv-3: peer 0000000000000004:0000000000000000:0000000000000000:0000000000000000 bits:1048343 flags:2024
[Tue Nov 17 20:06:58 2026] drbd p14/0 drbd1014 lima-dv-3: uuid_compare()=source-set-bitmap by rule=just-created-peer
[Tue Nov 17 20:06:58 2026] drbd p14/0 drbd1014 lima-dv-3: Setting and writing one bitmap slot, after drbd_sync_handshake
[Tue Nov 17 20:06:58 2026] drbd p14: State change 2401650196: primary_nodes=1, weak_nodes=FFFFFFFFFFFFFFF0
[Tue Nov 17 20:06:58 2026] drbd p14: Committing cluster-wide state change 2401650196 (16ms)
[Tue Nov 17 20:06:58 2026] drbd p14 lima-dv-3: conn( Connecting -> Connected ) peer( Unknown -> Secondary ) [connected]
[Tue Nov 17 20:06:58 2026] drbd p14/0 drbd1014 lima-dv-3: pdsk( DUnknown -> Inconsistent ) repl( Off -> WFBitMapS ) replication( yes -> no ) [connected]
[Tue Nov 17 20:06:58 2026] drbd p14/0 drbd1014 lima-dv-3: send bitmap stats [Bytes(packets)]: plain 0(0), RLE 21(1), total 21; compression: 100.0%
[Tue Nov 17 20:06:58 2026] drbd p14/0 drbd1014 lima-dv-3: receive bitmap stats [Bytes(packets)]: plain 0(0), RLE 21(1), total 21; compression: 100.0%
[Tue Nov 17 20:06:58 2026] drbd p14/0 drbd1014 lima-dv-3: repl( WFBitMapS -> SyncSource ) [receive-bitmap]
[Tue Nov 17 20:06:58 2026] drbd p14/0 drbd1014 lima-dv-3: Began resync as SyncSource (will sync 4193372 KB [1048343 bits set]).
[Tue Nov 17 20:06:59 2026] drbd p14: Preparing remote state change 2720487814: 1->2 role( Secondary ) conn( Connected )
[Tue Nov 17 20:06:59 2026] drbd p14 lima-dv-2: Committing remote state change 2720487814 (primary_nodes=1)
[Tue Nov 17 20:07:00 2026] drbd p14: Preparing remote state change 1026419957: 2->3 role( Secondary ) conn( Connected )
[Tue Nov 17 20:07:00 2026] drbd p14 lima-dv-3: Committing remote state change 1026419957 (primary_nodes=1)
[Tue Nov 17 20:07:01 2026] drbd p14: Preparing remote state change 1339369820: 1->0 conn( Disconnecting ) disk( Outdated )
[Tue Nov 17 20:07:01 2026] drbd p14 lima-dv-2: Committing remote state change 1339369820 (primary_nodes=1)
[Tue Nov 17 20:07:01 2026] drbd p14 lima-dv-2: conn( Connected -> TearDown ) peer( Secondary -> Unknown ) [remote]
[Tue Nov 17 20:07:01 2026] drbd p14/0 drbd1014 lima-dv-2: pdsk( UpToDate -> Outdated ) repl( Established -> Off ) [remote]
[Tue Nov 17 20:07:01 2026] drbd p14/0 drbd1014: gen-obligation: DISCHARGED(promoted) -> ARMED(peer-data-lost)
[Tue Nov 17 20:07:01 2026] drbd p14 lima-dv-2: Terminating sender thread
[Tue Nov 17 20:07:01 2026] drbd p14 lima-dv-2: Starting sender thread (peer-node-id 1)
[Tue Nov 17 20:07:01 2026] drbd p14/0 drbd1014: gen-obligation: ARMED(peer-data-lost) -> MINTING(peer-data-lost)
[Tue Nov 17 20:07:01 2026] drbd p14/0 drbd1014: new current UUID: E5DFAAFD812639C3 weak: FFFFFFFFFFFFFFFA
[Tue Nov 17 20:07:01 2026] drbd p14/0 drbd1014: gen-obligation: MINTING(peer-data-lost) -> DISCHARGED(peer-data-lost)
[Tue Nov 17 20:07:01 2026] drbd p14 lima-dv-2: Connection closed
[Tue Nov 17 20:07:01 2026] drbd p14 lima-dv-2: conn( TearDown -> Unconnected ) [disconnected]
[Tue Nov 17 20:07:01 2026] drbd p14 lima-dv-2: Restarting receiver thread
[Tue Nov 17 20:07:01 2026] drbd p14 lima-dv-2: conn( Unconnected -> Connecting ) [connecting]
[Tue Nov 17 20:07:01 2026] drbd p14: Preparing remote state change 2585014869: 1->3 conn( Disconnecting )
[Tue Nov 17 20:07:02 2026] drbd p14 lima-dv-3: Duplicate prepare for remote state change 2585014869
[Tue Nov 17 20:07:02 2026] drbd p14 lima-dv-4: Committing remote state change 2585014869 (primary_nodes=1)
[Tue Nov 17 20:07:03 2026] drbd p14: Preparing remote state change 1186503039: 1->2 conn( Disconnecting )
[Tue Nov 17 20:07:04 2026] drbd p14 lima-dv-3: Committing remote state change 1186503039 (primary_nodes=1)
[Tue Nov 17 20:07:19 2026] drbd p14/0 drbd1014: md_sync_timer expired! Worker calls drbd_md_sync().
[Tue Nov 17 20:07:28 2026] drbd p14 lima-dv-2: Handshake to peer 1 successful: Agreed network protocol version 125
[Tue Nov 17 20:07:28 2026] drbd p14 lima-dv-2: Feature flags enabled on protocol level: 0x7ff TRIM THIN_RESYNC WRITE_SAME WRITE_ZEROES RESYNC_DAGTAG WRITE_POSTPONE
[Tue Nov 17 20:07:28 2026] drbd p14: Preparing remote state change 2697809335: 1->3 role( Secondary ) conn( Connected )
[Tue Nov 17 20:07:28 2026] drbd p14 lima-dv-4: Committing remote state change 2697809335 (primary_nodes=1)
[Tue Nov 17 20:07:28 2026] drbd p14: Preparing cluster-wide state change 1154136345: 0->1 role( Primary ) conn( Connected )
[Tue Nov 17 20:07:28 2026] drbd p14/0 drbd1014 lima-dv-2: drbd_sync_handshake:
[Tue Nov 17 20:07:28 2026] drbd p14/0 drbd1014 lima-dv-2: self E5DFAAFD812639C3:23C2AA16826F4B7E:0000000000000000:0000000000000000 bits:16428 flags:2120
[Tue Nov 17 20:07:28 2026] drbd p14/0 drbd1014 lima-dv-2: peer 23C2AA16826F4B7E:0000000000000000:0000000000000000:0000000000000000 bits:0 flags:20
[Tue Nov 17 20:07:28 2026] drbd p14/0 drbd1014 lima-dv-2: uuid_compare()=source-use-bitmap by rule=bitmap-self
[Tue Nov 17 20:07:28 2026] drbd p14: State change 1154136345: primary_nodes=1, weak_nodes=FFFFFFFFFFFFFFF0
[Tue Nov 17 20:07:28 2026] drbd p14: Committing cluster-wide state change 1154136345 (16ms)
[Tue Nov 17 20:07:28 2026] drbd p14 lima-dv-2: conn( Connecting -> Connected ) peer( Unknown -> Secondary ) [connected]
[Tue Nov 17 20:07:28 2026] drbd p14/0 drbd1014 lima-dv-2: repl( Off -> WFBitMapS ) [connected]
[Tue Nov 17 20:07:28 2026] drbd p14/0 drbd1014 lima-dv-2: send bitmap stats [Bytes(packets)]: plain 0(0), RLE 40(1), total 40; compression: 100.0%
[Tue Nov 17 20:07:28 2026] drbd p14/0 drbd1014 lima-dv-2: receive bitmap stats [Bytes(packets)]: plain 0(0), RLE 40(1), total 40; compression: 100.0%
[Tue Nov 17 20:07:28 2026] drbd p14/0 drbd1014 lima-dv-2: pdsk( Outdated -> Inconsistent ) repl( WFBitMapS -> SyncSource ) replication( yes -> no ) [receive-bitmap]
[Tue Nov 17 20:07:28 2026] drbd p14/0 drbd1014 lima-dv-2: Began resync as SyncSource (will sync 65712 KB [16428 bits set]).
[Tue Nov 17 20:07:28 2026] drbd p14: Preparing remote state change 2853836342: 1->2 role( Secondary ) conn( Connected )
[Tue Nov 17 20:07:28 2026] drbd p14 lima-dv-2: Committing remote state change 2853836342 (primary_nodes=1)
[Tue Nov 17 20:07:35 2026] drbd p14/0 drbd1014 lima-dv-2: replication( no -> yes ) [enable-replication]
[Tue Nov 17 20:10:15 2026] drbd p14/0 drbd1014 lima-dv-2: updated UUIDs E5DFAAFD812639C3:0000000000000000:23C2AA16826F4B7E:0000000000000000
[Tue Nov 17 20:10:15 2026] drbd p14/0 drbd1014 lima-dv-2: Resync done (total 167 sec; paused 0 sec; 628 K/sec)
[Tue Nov 17 20:10:15 2026] drbd p14/0 drbd1014 lima-dv-2: pdsk( Inconsistent -> UpToDate ) repl( SyncSource -> Established ) [resync-finished]
[Tue Nov 17 20:16:38 2026] drbd p14/0 drbd1014 lima-dv-3: replication( no -> yes ) [enable-replication]
[Tue Nov 17 20:16:39 2026] drbd p14/0 drbd1014 lima-dv-3: updated UUIDs E5DFAAFD812639C3:0000000000000000:23C2AA16826F4B7E:0000000000000000
[Tue Nov 17 20:16:39 2026] drbd p14/0 drbd1014 lima-dv-3: Resync done (total 581 sec; paused 0 sec; 7392 K/sec)
[Tue Nov 17 20:16:39 2026] drbd p14/0 drbd1014 lima-dv-3: pdsk( Inconsistent -> UpToDate ) repl( SyncSource -> Established ) [resync-finished]
[Tue Nov 17 20:16:55 2026] drbd p14: Preparing cluster-wide state change 4153388161: 0->all role( Secondary )
[Tue Nov 17 20:16:55 2026] drbd p14: State change 4153388161: primary_nodes=0, weak_nodes=0
[Tue Nov 17 20:16:55 2026] drbd p14: Committing cluster-wide state change 4153388161 (0ms)
[Tue Nov 17 20:16:55 2026] drbd p14: role( Primary -> Secondary ) [umount:690777 auto-demote]
[Tue Nov 17 20:16:55 2026] drbd p14/0 drbd1014 lima-dv-3: pdsk( UpToDate -> Outdated ) [peer-state]
[Tue Nov 17 20:16:55 2026] drbd p14/0 drbd1014 lima-dv-3: pdsk( Outdated -> Inconsistent ) resync-susp( no -> peer ) replication( yes -> no ) [peer-state]

### P14 variant with a witness, run 2
$ [dv-1] drbdadm create-md --force --max-peers=7 p14
initializing bitmap (896 KB) to all zero
initializing activity log
Writing meta data...
New drbd meta data block successfully created.
rc=0
$ [dv-2] drbdadm create-md --force --max-peers=7 p14
initializing bitmap (896 KB) to all zero
initializing activity log
Writing meta data...
New drbd meta data block successfully created.
rc=0
$ [dv-1] drbdadm up p14
rc=0
$ [dv-2] drbdadm up p14
rc=0
$ [dv-4] drbdadm up p14
rc=0
wait: dv-1: 2 connections Connected after 2 s at most
wait: dv-2: 2 connections Connected after 2 s at most
wait: dv-4: 2 connections Connected after 2 s at most
$ [dv-1] drbdadm new-current-uuid --clear-bitmap p14/0
rc=0
$ [dv-1] drbdsetup status p14 --verbose
p14 node-id:0 role:Secondary suspended:no force-io-failures:no
  volume:0 minor:1014 disk:UpToDate backing_dev:/dev/ambar-exp/p14 quorum:yes
      open:no blocked:no
  lima-dv-2 node-id:1 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
  lima-dv-4 node-id:3 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:Diskless peer-client:yes
        resync-suspended:no

rc=0
$ [dv-1] dd if=/dev/urandom of=/dev/drbd1014 bs=1M count=2048 oflag=direct status=none
rc=0
$ [dv-1] mkfs.ext4 -q -F -E nodiscard /dev/drbd1014
rc=0
20:18:13.924
2361
started
free space on dv-1 after the 2 GiB write: /dev/vda1        20G  4.4G   15G  24% /

### P14: dv-3 joins with a copy
$ [dv-3] drbdadm create-md --force --max-peers=7 p14
initializing bitmap (896 KB) to all zero
initializing activity log
Writing meta data...
New drbd meta data block successfully created.
rc=0
$ [dv-3] drbdadm up p14
rc=0
@ 20:18:18.472 up on dv-3 (UP_T0)
$ [dv-1] drbdadm adjust p14
rc=0
$ [dv-2] drbdadm adjust p14
rc=0
$ [dv-4] drbdadm adjust p14
rc=0

### P14: wait for dv-3 Inconsistent, then down on dv-2
p14 node-id:0 role:Primary suspended:no force-io-failures:no
    write-ordering:flush
  volume:0 minor:1014 disk:UpToDate backing_dev:/dev/ambar-exp/p14 quorum:yes
      open:yes
      size:4193372 read:15757 written:2344168 al-writes:522 bm-writes:453
      upper-pending:1 lower-pending:1 al-suspended:no blocked:no
  lima-dv-2 node-id:1 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:8 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
        received:0 sent:2213000 out-of-sync:0 pending:0 unacked:0
  lima-dv-3 node-id:2 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:SyncSource peer-disk:Inconsistent done:0.33
        resync-suspended:no
        received:0 sent:13952 out-of-sync:4179420 pending:0 unacked:1 dbdt1:6.14 eta:665
  lima-dv-4 node-id:3 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:Diskless peer-client:yes
        resync-suspended:no
        received:0 sent:0 out-of-sync:0 pending:0 unacked:0
@ 20:18:23.856 dv-3 seen Inconsistent (t+5 s after up); down on dv-2
$ [dv-2] drbdadm down p14
rc=0
@ 20:18:27.342 down on dv-2 returned
$ [dv-1] drbdsetup status p14 --verbose --statistics
p14 node-id:0 role:Primary suspended:no force-io-failures:no
    write-ordering:flush
  volume:0 minor:1014 disk:UpToDate backing_dev:/dev/ambar-exp/p14 quorum:yes
      open:yes
      size:4193372 read:18541 written:2402940 al-writes:522 bm-writes:456
      upper-pending:0 lower-pending:1 al-suspended:no blocked:no
  lima-dv-2 node-id:1 connection:Connecting role:Unknown tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Off peer-disk:Outdated resync-suspended:no
        received:0 sent:0 out-of-sync:26324 pending:0 unacked:0
  lima-dv-3 node-id:2 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:SyncSource peer-disk:Inconsistent done:0.40
        resync-suspended:no
        received:0 sent:16736 out-of-sync:4176636 pending:0 unacked:1 dbdt1:0.36
        eta:11227
  lima-dv-4 node-id:3 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:Diskless peer-client:yes
        resync-suspended:no
        received:0 sent:0 out-of-sync:0 pending:0 unacked:0

rc=0
@ 20:18:48.367 20 s after down on dv-2
$ [dv-1] drbdsetup status p14 --verbose --statistics
p14 node-id:0 role:Primary suspended:no force-io-failures:no
    write-ordering:flush
  volume:0 minor:1014 disk:UpToDate backing_dev:/dev/ambar-exp/p14 quorum:yes
      open:yes
      size:4193372 read:75533 written:2787604 al-writes:522 bm-writes:469
      upper-pending:1 lower-pending:2 al-suspended:no blocked:no
  lima-dv-2 node-id:1 connection:Connecting role:Unknown tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Off peer-disk:Outdated resync-suspended:no
        received:0 sent:0 out-of-sync:65708 pending:0 unacked:0
  lima-dv-3 node-id:2 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:SyncSource peer-disk:Inconsistent done:1.76
        resync-suspended:no
        received:0 sent:73728 out-of-sync:4119656 pending:0 unacked:1 dbdt1:1.53 eta:2629
  lima-dv-4 node-id:3 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:Diskless peer-client:yes
        resync-suspended:no
        received:0 sent:0 out-of-sync:0 pending:0 unacked:0

rc=0
$ [dv-3] drbdsetup status p14 --verbose --statistics
p14 node-id:2 role:Secondary suspended:no force-io-failures:no
    write-ordering:flush
  volume:0 minor:1014 disk:Inconsistent backing_dev:/dev/ambar-exp/p14 quorum:yes
      open:no
      size:4193372 read:0 written:81920 al-writes:0 bm-writes:244 upper-pending:0
      lower-pending:0 al-suspended:no blocked:no
  lima-dv-1 node-id:0 connection:Connected role:Primary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:SyncTarget peer-disk:UpToDate done:1.95 resync-suspended:no
        received:81920 sent:0 out-of-sync:4111464 pending:9 unacked:0 dbdt1:3.01 eta:1336
  lima-dv-2 node-id:1 connection:Connecting role:Unknown tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Off peer-disk:Outdated resync-suspended:dependency
        received:0 sent:0 out-of-sync:4189276 pending:0 unacked:0
  lima-dv-4 node-id:3 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:Diskless peer-client:yes
        resync-suspended:dependency
        received:0 sent:0 out-of-sync:0 pending:0 unacked:0

rc=0

### P14: dv-2 returns
$ [dv-2] drbdadm up p14
rc=0
@ 20:18:51.312 up on dv-2
wait: dv-2 UpToDate after 88 s at most
wait: dv-3 UpToDate after 144 s at most
@ 20:24:50.159 all copies UpToDate
$ [dv-1] drbdsetup status p14 --verbose --statistics
p14 node-id:0 role:Primary suspended:no force-io-failures:no
    write-ordering:flush
  volume:0 minor:1014 disk:UpToDate backing_dev:/dev/ambar-exp/p14 quorum:yes
      open:yes
      size:4193372 read:4392669 written:5304964 al-writes:522 bm-writes:876
      upper-pending:0 lower-pending:0 al-suspended:no blocked:no
  lima-dv-2 node-id:1 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:8 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
        received:0 sent:2415988 out-of-sync:0 pending:0 unacked:0
  lima-dv-3 node-id:2 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:8 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
        received:0 sent:4859548 out-of-sync:0 pending:0 unacked:0
  lima-dv-4 node-id:3 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:Diskless peer-client:yes
        resync-suspended:no
        received:0 sent:0 out-of-sync:0 pending:0 unacked:0

rc=0
$ [dv-2] drbdsetup status p14 --verbose --statistics
p14 node-id:1 role:Secondary suspended:no force-io-failures:no
    write-ordering:flush
  volume:0 minor:1014 disk:UpToDate backing_dev:/dev/ambar-exp/p14 quorum:yes
      open:no
      size:4193372 read:0 written:2421856 al-writes:18 bm-writes:379 upper-pending:0
      lower-pending:1 al-suspended:no blocked:no
  lima-dv-1 node-id:0 connection:Connected role:Primary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
        received:2421860 sent:0 out-of-sync:0 pending:0 unacked:1
  lima-dv-3 node-id:2 connection:StandAlone role:Unknown tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Off peer-disk:Outdated resync-suspended:no
        received:0 sent:0 out-of-sync:81684 pending:0 unacked:0
  lima-dv-4 node-id:3 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:Diskless peer-client:yes
        resync-suspended:no
        received:0 sent:0 out-of-sync:0 pending:0 unacked:0

rc=0
$ [dv-3] drbdsetup status p14 --verbose --statistics
p14 node-id:2 role:Secondary suspended:no force-io-failures:no
    write-ordering:flush
  volume:0 minor:1014 disk:UpToDate backing_dev:/dev/ambar-exp/p14 quorum:yes
      open:no
      size:4193372 read:0 written:4871148 al-writes:18 bm-writes:741 upper-pending:0
      lower-pending:1 al-suspended:no blocked:no
  lima-dv-1 node-id:0 connection:Connected role:Primary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
        received:4871152 sent:0 out-of-sync:0 pending:0 unacked:1
  lima-dv-2 node-id:1 connection:StandAlone role:Unknown tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Off peer-disk:Outdated resync-suspended:no
        received:0 sent:0 out-of-sync:81684 pending:0 unacked:0
  lima-dv-4 node-id:3 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:Diskless peer-client:yes
        resync-suspended:no
        received:0 sent:0 out-of-sync:0 pending:0 unacked:0

rc=0
$ [dv-4] drbdsetup status p14 --verbose --statistics
p14 node-id:3 role:Secondary suspended:no force-io-failures:no
    write-ordering:none
  volume:0 minor:1014 disk:Diskless client:yes backing_dev:none quorum:yes open:no
      size:4193372 read:0 written:0 al-writes:0 bm-writes:0 upper-pending:0
      lower-pending:0 al-suspended:no blocked:no
  lima-dv-1 node-id:0 connection:Connected role:Primary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
        received:0 sent:0 out-of-sync:0 pending:0 unacked:0
  lima-dv-2 node-id:1 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
        received:0 sent:0 out-of-sync:0 pending:0 unacked:0
  lima-dv-3 node-id:2 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
        received:0 sent:0 out-of-sync:0 pending:0 unacked:0

rc=0
@ 20:24:54.035 stop the writer
acked=192945 on-volume=192946
--- probe.sh log (stdout and stderr)
--- watch.sh log
samples=793 probe=alive:R=66 probe=alive:S=727 longest-gap-without-new-acked=0.7s (from 20:21:36.030)
--- quorum log of dv-1 (runs of equal values: first - last  value  samples)
20:18:15.121 - 20:24:55.280  "quorum": true  x399
--- dmesg -T | grep 'drbd p14' | tail -n 80 [dv-1]
[Tue Nov 17 20:18:06 2026] drbd p14/0 drbd1014: gen-obligation: DISCHARGED(promoted) -> ARMED(promoted)
[Tue Nov 17 20:18:06 2026] drbd p14/0 drbd1014: gen-obligation: ARMED(promoted) -> MINTING(promoted)
[Tue Nov 17 20:18:06 2026] drbd p14/0 drbd1014: gen-obligation: MINTING(promoted) -> DISCHARGED(promoted)
[Tue Nov 17 20:18:18 2026] drbd p14 lima-dv-3: Starting sender thread (peer-node-id 2)
[Tue Nov 17 20:18:18 2026] drbd p14 lima-dv-3: conn( StandAlone -> Unconnected ) [connect]
[Tue Nov 17 20:18:18 2026] drbd p14 lima-dv-3: Starting receiver thread (peer-node-id 2)
[Tue Nov 17 20:18:18 2026] drbd p14 lima-dv-3: conn( Unconnected -> Connecting ) [connecting]
[Tue Nov 17 20:18:19 2026] drbd p14 lima-dv-3: Handshake to peer 2 successful: Agreed network protocol version 125
[Tue Nov 17 20:18:19 2026] drbd p14 lima-dv-3: Feature flags enabled on protocol level: 0x7ff TRIM THIN_RESYNC WRITE_SAME WRITE_ZEROES RESYNC_DAGTAG WRITE_POSTPONE
[Tue Nov 17 20:18:19 2026] drbd p14: Preparing cluster-wide state change 807019812: 0->2 role( Primary ) conn( Connected )
[Tue Nov 17 20:18:19 2026] drbd p14/0 drbd1014 lima-dv-3: drbd_sync_handshake:
[Tue Nov 17 20:18:19 2026] drbd p14/0 drbd1014 lima-dv-3: self 7F149FDE6D00D0E0:F84A6487D0F99171:0000000000000000:0000000000000000 bits:532511 flags:20
[Tue Nov 17 20:18:19 2026] drbd p14/0 drbd1014 lima-dv-3: peer 0000000000000004:0000000000000000:0000000000000000:0000000000000000 bits:1048343 flags:2024
[Tue Nov 17 20:18:19 2026] drbd p14/0 drbd1014 lima-dv-3: uuid_compare()=source-set-bitmap by rule=just-created-peer
[Tue Nov 17 20:18:19 2026] drbd p14/0 drbd1014 lima-dv-3: Setting and writing one bitmap slot, after drbd_sync_handshake
[Tue Nov 17 20:18:19 2026] drbd p14: State change 807019812: primary_nodes=1, weak_nodes=FFFFFFFFFFFFFFF0
[Tue Nov 17 20:18:19 2026] drbd p14: Committing cluster-wide state change 807019812 (16ms)
[Tue Nov 17 20:18:19 2026] drbd p14 lima-dv-3: conn( Connecting -> Connected ) peer( Unknown -> Secondary ) [connected]
[Tue Nov 17 20:18:19 2026] drbd p14/0 drbd1014 lima-dv-3: pdsk( DUnknown -> Inconsistent ) repl( Off -> WFBitMapS ) replication( yes -> no ) [connected]
[Tue Nov 17 20:18:19 2026] drbd p14/0 drbd1014 lima-dv-3: send bitmap stats [Bytes(packets)]: plain 0(0), RLE 21(1), total 21; compression: 100.0%
[Tue Nov 17 20:18:19 2026] drbd p14/0 drbd1014 lima-dv-3: receive bitmap stats [Bytes(packets)]: plain 0(0), RLE 21(1), total 21; compression: 100.0%
[Tue Nov 17 20:18:19 2026] drbd p14/0 drbd1014 lima-dv-3: repl( WFBitMapS -> SyncSource ) [receive-bitmap]
[Tue Nov 17 20:18:19 2026] drbd p14/0 drbd1014 lima-dv-3: Began resync as SyncSource (will sync 4193372 KB [1048343 bits set]).
[Tue Nov 17 20:18:20 2026] drbd p14: Preparing remote state change 395868828: 1->2 role( Secondary ) conn( Connected )
[Tue Nov 17 20:18:20 2026] drbd p14 lima-dv-2: Committing remote state change 395868828 (primary_nodes=1)
[Tue Nov 17 20:18:21 2026] drbd p14: Preparing remote state change 3701276143: 2->3 role( Secondary ) conn( Connected )
[Tue Nov 17 20:18:22 2026] drbd p14 lima-dv-2: Rejecting concurrent remote state change 196304226 because of state change 3701276143
[Tue Nov 17 20:18:22 2026] drbd p14 lima-dv-2: Rejecting concurrent remote state change 3038753799 because of state change 3701276143
[Tue Nov 17 20:18:22 2026] drbd p14 lima-dv-3: Committing remote state change 3701276143 (primary_nodes=1)
[Tue Nov 17 20:18:23 2026] drbd p14: Preparing remote state change 963252877: 1->0 conn( Disconnecting ) disk( Outdated )
[Tue Nov 17 20:18:23 2026] drbd p14 lima-dv-2: Committing remote state change 963252877 (primary_nodes=1)
[Tue Nov 17 20:18:23 2026] drbd p14 lima-dv-2: conn( Connected -> TearDown ) peer( Secondary -> Unknown ) [remote]
[Tue Nov 17 20:18:23 2026] drbd p14/0 drbd1014 lima-dv-2: pdsk( UpToDate -> Outdated ) repl( Established -> Off ) [remote]
[Tue Nov 17 20:18:23 2026] drbd p14/0 drbd1014: gen-obligation: DISCHARGED(promoted) -> ARMED(peer-data-lost)
[Tue Nov 17 20:18:23 2026] drbd p14 lima-dv-2: Terminating sender thread
[Tue Nov 17 20:18:23 2026] drbd p14 lima-dv-2: Starting sender thread (peer-node-id 1)
[Tue Nov 17 20:18:23 2026] drbd p14/0 drbd1014: gen-obligation: ARMED(peer-data-lost) -> MINTING(peer-data-lost)
[Tue Nov 17 20:18:23 2026] drbd p14/0 drbd1014: new current UUID: 2BB675C5D66A8875 weak: FFFFFFFFFFFFFFFA
[Tue Nov 17 20:18:23 2026] drbd p14/0 drbd1014: gen-obligation: MINTING(peer-data-lost) -> DISCHARGED(peer-data-lost)
[Tue Nov 17 20:18:23 2026] drbd p14 lima-dv-2: Connection closed
[Tue Nov 17 20:18:23 2026] drbd p14 lima-dv-2: conn( TearDown -> Unconnected ) [disconnected]
[Tue Nov 17 20:18:23 2026] drbd p14 lima-dv-2: Restarting receiver thread
[Tue Nov 17 20:18:23 2026] drbd p14 lima-dv-2: conn( Unconnected -> Connecting ) [connecting]
[Tue Nov 17 20:18:24 2026] drbd p14: Preparing remote state change 2714837418: 1->3 conn( Disconnecting )
[Tue Nov 17 20:18:24 2026] drbd p14 lima-dv-3: Duplicate prepare for remote state change 2714837418
[Tue Nov 17 20:18:24 2026] drbd p14 lima-dv-4: Committing remote state change 2714837418 (primary_nodes=1)
[Tue Nov 17 20:18:24 2026] drbd p14: Preparing remote state change 1354255287: 1->2 conn( Disconnecting )
[Tue Nov 17 20:18:25 2026] drbd p14 lima-dv-3: Committing remote state change 1354255287 (primary_nodes=1)
[Tue Nov 17 20:18:31 2026] drbd p14/0 drbd1014: md_sync_timer expired! Worker calls drbd_md_sync().
[Tue Nov 17 20:18:49 2026] drbd p14 lima-dv-2: Handshake to peer 1 successful: Agreed network protocol version 125
[Tue Nov 17 20:18:49 2026] drbd p14 lima-dv-2: Feature flags enabled on protocol level: 0x7ff TRIM THIN_RESYNC WRITE_SAME WRITE_ZEROES RESYNC_DAGTAG WRITE_POSTPONE
[Tue Nov 17 20:18:49 2026] drbd p14: Preparing cluster-wide state change 1363876714: 0->1 role( Primary ) conn( Connected )
[Tue Nov 17 20:18:49 2026] drbd p14/0 drbd1014 lima-dv-2: drbd_sync_handshake:
[Tue Nov 17 20:18:49 2026] drbd p14/0 drbd1014 lima-dv-2: self 2BB675C5D66A8875:7F149FDE6D00D0E0:0000000000000000:0000000000000000 bits:16430 flags:2120
[Tue Nov 17 20:18:49 2026] drbd p14/0 drbd1014 lima-dv-2: peer 7F149FDE6D00D0E0:0000000000000000:0000000000000000:0000000000000000 bits:0 flags:20
[Tue Nov 17 20:18:49 2026] drbd p14/0 drbd1014 lima-dv-2: uuid_compare()=source-use-bitmap by rule=bitmap-self
[Tue Nov 17 20:18:49 2026] drbd p14: State change 1363876714: primary_nodes=1, weak_nodes=FFFFFFFFFFFFFFF0
[Tue Nov 17 20:18:49 2026] drbd p14: Committing cluster-wide state change 1363876714 (16ms)
[Tue Nov 17 20:18:49 2026] drbd p14 lima-dv-2: conn( Connecting -> Connected ) peer( Unknown -> Secondary ) [connected]
[Tue Nov 17 20:18:49 2026] drbd p14/0 drbd1014 lima-dv-2: repl( Off -> WFBitMapS ) [connected]
[Tue Nov 17 20:18:49 2026] drbd p14/0 drbd1014 lima-dv-2: send bitmap stats [Bytes(packets)]: plain 0(0), RLE 47(1), total 47; compression: 100.0%
[Tue Nov 17 20:18:49 2026] drbd p14: Preparing remote state change 3831557618: 1->2 role( Secondary ) conn( Connected )
[Tue Nov 17 20:18:49 2026] drbd p14/0 drbd1014 lima-dv-2: receive bitmap stats [Bytes(packets)]: plain 0(0), RLE 47(1), total 47; compression: 100.0%
[Tue Nov 17 20:18:49 2026] drbd p14/0 drbd1014 lima-dv-2: pdsk( Outdated -> Inconsistent ) repl( WFBitMapS -> SyncSource ) replication( yes -> no ) [receive-bitmap]
[Tue Nov 17 20:18:49 2026] drbd p14/0 drbd1014 lima-dv-2: Began resync as SyncSource (will sync 65720 KB [16430 bits set]).
[Tue Nov 17 20:18:49 2026] drbd p14 lima-dv-2: Aborting remote state change 3831557618
[Tue Nov 17 20:18:49 2026] drbd p14: Preparing remote state change 920034239: 1->3 role( Secondary ) conn( Connected )
[Tue Nov 17 20:18:49 2026] drbd p14 lima-dv-2: Committing remote state change 920034239 (primary_nodes=1)
[Tue Nov 17 20:18:57 2026] drbd p14/0 drbd1014 lima-dv-2: replication( no -> yes ) [enable-replication]
[Tue Nov 17 20:20:58 2026] drbd p14/0 drbd1014 lima-dv-2: updated UUIDs 2BB675C5D66A8875:0000000000000000:7F149FDE6D00D0E0:0000000000000000
[Tue Nov 17 20:20:58 2026] drbd p14/0 drbd1014 lima-dv-2: Resync done (total 128 sec; paused 0 sec; 820 K/sec)
[Tue Nov 17 20:20:58 2026] drbd p14/0 drbd1014 lima-dv-2: pdsk( Inconsistent -> UpToDate ) repl( SyncSource -> Established ) [resync-finished]
[Tue Nov 17 20:23:16 2026] drbd p14/0 drbd1014 lima-dv-3: replication( no -> yes ) [enable-replication]
[Tue Nov 17 20:24:44 2026] drbd p14/0 drbd1014 lima-dv-3: updated UUIDs 2BB675C5D66A8875:0000000000000000:7F149FDE6D00D0E0:0000000000000000
[Tue Nov 17 20:24:44 2026] drbd p14/0 drbd1014 lima-dv-3: Resync done (total 385 sec; paused 0 sec; 11240 K/sec)
[Tue Nov 17 20:24:44 2026] drbd p14/0 drbd1014 lima-dv-3: pdsk( Inconsistent -> UpToDate ) repl( SyncSource -> Established ) [resync-finished]
[Tue Nov 17 20:24:53 2026] drbd p14: Preparing cluster-wide state change 1088711679: 0->all role( Secondary )
[Tue Nov 17 20:24:53 2026] drbd p14: State change 1088711679: primary_nodes=0, weak_nodes=0
[Tue Nov 17 20:24:53 2026] drbd p14: Committing cluster-wide state change 1088711679 (0ms)
[Tue Nov 17 20:24:53 2026] drbd p14: role( Primary -> Secondary ) [umount:909627 auto-demote]

### P14 variant without a witness, run 1
$ [dv-1] drbdadm create-md --force --max-peers=7 p14
initializing bitmap (896 KB) to all zero
initializing activity log
Writing meta data...
New drbd meta data block successfully created.
rc=0
$ [dv-2] drbdadm create-md --force --max-peers=7 p14
initializing bitmap (896 KB) to all zero
initializing activity log
Writing meta data...
New drbd meta data block successfully created.
rc=0
$ [dv-1] drbdadm up p14
rc=0
$ [dv-2] drbdadm up p14
rc=0
wait: dv-1: 1 connections Connected after 1 s
wait: dv-2: 1 connections Connected after 1 s
$ [dv-1] drbdadm new-current-uuid --clear-bitmap p14/0
rc=0
$ [dv-1] drbdsetup status p14 --verbose
p14 node-id:0 role:Secondary suspended:no force-io-failures:no
  volume:0 minor:1014 disk:UpToDate backing_dev:/dev/ambar-exp/p14 quorum:yes
      open:no blocked:no
  lima-dv-2 node-id:1 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no

rc=0
$ [dv-1] dd if=/dev/urandom of=/dev/drbd1014 bs=1M count=2048 oflag=direct status=none
rc=0
$ [dv-1] mkfs.ext4 -q -F -E nodiscard /dev/drbd1014
rc=0
20:39:42.124
1724
started
free space on dv-1 after the 2 GiB write: /dev/vda1        20G  4.4G   15G  24% /

### P14: dv-3 joins with a copy
$ [dv-3] drbdadm create-md --force --max-peers=7 p14
initializing bitmap (896 KB) to all zero
initializing activity log
Writing meta data...
New drbd meta data block successfully created.
rc=0
$ [dv-3] drbdadm up p14
rc=0
@ 20:39:47.275 up on dv-3 (UP_T0)
$ [dv-1] drbdadm adjust p14
rc=0
$ [dv-2] drbdadm adjust p14
rc=0

### P14: wait for dv-3 Inconsistent, then down on dv-2
p14 node-id:0 role:Primary suspended:no force-io-failures:no
    write-ordering:flush
  volume:0 minor:1014 disk:UpToDate backing_dev:/dev/ambar-exp/p14 quorum:yes
      open:yes
      size:4193372 read:65293 written:2304552 al-writes:522 bm-writes:452
      upper-pending:0 lower-pending:1 al-suspended:no blocked:no
  lima-dv-2 node-id:1 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
        received:0 sent:2173388 out-of-sync:0 pending:1 unacked:0
  lima-dv-3 node-id:2 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:SyncSource peer-disk:Inconsistent done:1.51
        resync-suspended:no
        received:0 sent:63488 out-of-sync:4129884 pending:0 unacked:1 dbdt1:38.56 eta:105
@ 20:39:51.866 dv-3 seen Inconsistent (t+4 s after up); down on dv-2
$ [dv-2] drbdadm down p14
rc=0
@ 20:39:53.736 down on dv-2 returned
$ [dv-1] drbdsetup status p14 --verbose --statistics
p14 node-id:0 role:Primary suspended:no force-io-failures:no
    write-ordering:flush
  volume:0 minor:1014 disk:UpToDate backing_dev:/dev/ambar-exp/p14 quorum:yes
      open:yes
      size:4193372 read:87821 written:2328548 al-writes:522 bm-writes:459
      upper-pending:1 lower-pending:2 al-suspended:no blocked:no
  lima-dv-2 node-id:1 connection:Connecting role:Unknown tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Off peer-disk:Outdated resync-suspended:no
        received:0 sent:0 out-of-sync:13772 pending:0 unacked:0
  lima-dv-3 node-id:2 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:SyncSource peer-disk:Inconsistent done:2.05
        resync-suspended:no
        received:0 sent:86016 out-of-sync:4107356 pending:0 unacked:1 dbdt1:17.98 eta:223

rc=0
@ 20:40:15.002 20 s after down on dv-2
$ [dv-1] drbdsetup status p14 --verbose --statistics
p14 node-id:0 role:Primary suspended:no force-io-failures:no
    write-ordering:flush
  volume:0 minor:1014 disk:UpToDate backing_dev:/dev/ambar-exp/p14 quorum:yes
      open:yes
      size:4193372 read:151565 written:2616280 al-writes:522 bm-writes:472
      upper-pending:1 lower-pending:3 al-suspended:no blocked:no
  lima-dv-2 node-id:1 connection:Connecting role:Unknown tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Off peer-disk:Outdated resync-suspended:no
        received:0 sent:0 out-of-sync:65664 pending:0 unacked:0
  lima-dv-3 node-id:2 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:SyncSource peer-disk:Inconsistent done:3.57
        resync-suspended:no
        received:0 sent:149760 out-of-sync:4043624 pending:0 unacked:1 dbdt1:5.16 eta:766

rc=0
$ [dv-3] drbdsetup status p14 --verbose --statistics
p14 node-id:2 role:Secondary suspended:no force-io-failures:no
    write-ordering:flush
  volume:0 minor:1014 disk:Inconsistent backing_dev:/dev/ambar-exp/p14 quorum:yes
      open:no
      size:4193372 read:0 written:150080 al-writes:0 bm-writes:249 upper-pending:0
      lower-pending:0 al-suspended:no blocked:no
  lima-dv-1 node-id:0 connection:Connected role:Primary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:SyncTarget peer-disk:UpToDate done:3.58 resync-suspended:no
        received:150080 sent:0 out-of-sync:4043304 pending:3 unacked:0 dbdt1:4.21 eta:938
  lima-dv-2 node-id:1 connection:Connecting role:Unknown tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Off peer-disk:Outdated resync-suspended:dependency
        received:0 sent:0 out-of-sync:4176988 pending:0 unacked:0

rc=0

### P14: dv-2 returns
$ [dv-2] drbdadm up p14
rc=0
@ 20:40:18.434 up on dv-2
wait: dv-2 UpToDate after 59 s
wait: dv-3 UpToDate after 118 s
@ 20:43:18.985 all copies UpToDate
$ [dv-1] drbdsetup status p14 --verbose --statistics
p14 node-id:0 role:Primary suspended:no force-io-failures:no
    write-ordering:flush
  volume:0 minor:1014 disk:UpToDate backing_dev:/dev/ambar-exp/p14 quorum:yes
      open:yes
      size:4193372 read:4354433 written:3475520 al-writes:522 bm-writes:827
      upper-pending:1 lower-pending:1 al-suspended:no blocked:no
  lima-dv-2 node-id:1 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:8 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
        received:0 sent:824556 out-of-sync:0 pending:1 unacked:0
  lima-dv-3 node-id:2 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:8 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
        received:0 sent:4412732 out-of-sync:0 pending:1 unacked:0

rc=0
$ [dv-2] drbdsetup status p14 --verbose --statistics
p14 node-id:1 role:Secondary suspended:no force-io-failures:no
    write-ordering:flush
  volume:0 minor:1014 disk:UpToDate backing_dev:/dev/ambar-exp/p14 quorum:yes
      open:no
      size:4193372 read:0 written:830604 al-writes:19 bm-writes:338 upper-pending:0
      lower-pending:0 al-suspended:no blocked:no
  lima-dv-1 node-id:0 connection:Connected role:Primary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
        received:830604 sent:0 out-of-sync:0 pending:0 unacked:0
  lima-dv-3 node-id:2 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
        received:0 sent:0 out-of-sync:126924 pending:0 unacked:0

rc=0
$ [dv-3] drbdsetup status p14 --verbose --statistics
p14 node-id:2 role:Secondary suspended:no force-io-failures:no
    write-ordering:flush
  volume:0 minor:1014 disk:UpToDate backing_dev:/dev/ambar-exp/p14 quorum:yes
      open:no
      size:4193372 read:0 written:4424844 al-writes:18 bm-writes:633 upper-pending:0
      lower-pending:0 al-suspended:no blocked:no
  lima-dv-1 node-id:0 connection:Connected role:Primary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
        received:4424844 sent:0 out-of-sync:0 pending:0 unacked:0
  lima-dv-2 node-id:1 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
        received:0 sent:0 out-of-sync:126920 pending:0 unacked:0

rc=0
@ 20:43:21.906 stop the writer
acked=78462 on-volume=78463
--- probe.sh log (stdout and stderr)
--- watch.sh log
samples=438 probe=alive:R=25 probe=alive:S=413 longest-gap-without-new-acked=0.6s (from 20:42:23.851)
--- quorum log of dv-1 (runs of equal values: first - last  value  samples)
20:39:43.544 - 20:43:23.092  "quorum": true  x219
--- dmesg -T | grep 'drbd p14' | tail -n 80 [dv-1]
[Tue Nov 17 20:39:34 2026] drbd p14: State change 2972968910: primary_nodes=0, weak_nodes=0
[Tue Nov 17 20:39:34 2026] drbd p14: Committing cluster-wide state change 2972968910 (0ms)
[Tue Nov 17 20:39:34 2026] drbd p14: role( Primary -> Secondary ) [mkfs.ext4:1250183 auto-demote]
[Tue Nov 17 20:39:34 2026] drbd p14: Preparing cluster-wide state change 2661660735: 0->all role( Primary )
[Tue Nov 17 20:39:34 2026] drbd p14: State change 2661660735: primary_nodes=1, weak_nodes=FFFFFFFFFFFFFFFC
[Tue Nov 17 20:39:34 2026] drbd p14: Committing cluster-wide state change 2661660735 (0ms)
[Tue Nov 17 20:39:34 2026] drbd p14: role( Secondary -> Primary ) [mount:1250208 auto-promote]
[Tue Nov 17 20:39:34 2026] drbd p14/0 drbd1014: gen-obligation: DISCHARGED(promoted) -> ARMED(promoted)
[Tue Nov 17 20:39:34 2026] drbd p14/0 drbd1014: gen-obligation: ARMED(promoted) -> MINTING(promoted)
[Tue Nov 17 20:39:34 2026] drbd p14/0 drbd1014: gen-obligation: MINTING(promoted) -> DISCHARGED(promoted)
[Tue Nov 17 20:39:47 2026] drbd p14 lima-dv-3: Starting sender thread (peer-node-id 2)
[Tue Nov 17 20:39:47 2026] drbd p14 lima-dv-3: conn( StandAlone -> Unconnected ) [connect]
[Tue Nov 17 20:39:47 2026] drbd p14 lima-dv-3: Starting receiver thread (peer-node-id 2)
[Tue Nov 17 20:39:47 2026] drbd p14 lima-dv-3: conn( Unconnected -> Connecting ) [connecting]
[Tue Nov 17 20:39:47 2026] drbd p14 lima-dv-3: Handshake to peer 2 successful: Agreed network protocol version 125
[Tue Nov 17 20:39:47 2026] drbd p14 lima-dv-3: Feature flags enabled on protocol level: 0x7ff TRIM THIN_RESYNC WRITE_SAME WRITE_ZEROES RESYNC_DAGTAG WRITE_POSTPONE
[Tue Nov 17 20:39:47 2026] drbd p14: Preparing cluster-wide state change 1176290708: 0->2 role( Primary ) conn( Connected )
[Tue Nov 17 20:39:47 2026] drbd p14/0 drbd1014 lima-dv-3: drbd_sync_handshake:
[Tue Nov 17 20:39:47 2026] drbd p14/0 drbd1014 lima-dv-3: self 9A4210003A0A9AA4:DEA4C78192375721:0000000000000000:0000000000000000 bits:532511 flags:20
[Tue Nov 17 20:39:47 2026] drbd p14/0 drbd1014 lima-dv-3: peer 0000000000000004:0000000000000000:0000000000000000:0000000000000000 bits:1048343 flags:2024
[Tue Nov 17 20:39:47 2026] drbd p14/0 drbd1014 lima-dv-3: uuid_compare()=source-set-bitmap by rule=just-created-peer
[Tue Nov 17 20:39:47 2026] drbd p14/0 drbd1014 lima-dv-3: Setting and writing one bitmap slot, after drbd_sync_handshake
[Tue Nov 17 20:39:47 2026] drbd p14: State change 1176290708: primary_nodes=1, weak_nodes=FFFFFFFFFFFFFFF8
[Tue Nov 17 20:39:47 2026] drbd p14: Committing cluster-wide state change 1176290708 (20ms)
[Tue Nov 17 20:39:47 2026] drbd p14 lima-dv-3: conn( Connecting -> Connected ) peer( Unknown -> Secondary ) [connected]
[Tue Nov 17 20:39:47 2026] drbd p14/0 drbd1014 lima-dv-3: pdsk( DUnknown -> Inconsistent ) repl( Off -> WFBitMapS ) replication( yes -> no ) [connected]
[Tue Nov 17 20:39:47 2026] drbd p14/0 drbd1014 lima-dv-3: send bitmap stats [Bytes(packets)]: plain 0(0), RLE 21(1), total 21; compression: 100.0%
[Tue Nov 17 20:39:47 2026] drbd p14/0 drbd1014 lima-dv-3: receive bitmap stats [Bytes(packets)]: plain 0(0), RLE 21(1), total 21; compression: 100.0%
[Tue Nov 17 20:39:47 2026] drbd p14/0 drbd1014 lima-dv-3: repl( WFBitMapS -> SyncSource ) [receive-bitmap]
[Tue Nov 17 20:39:47 2026] drbd p14/0 drbd1014 lima-dv-3: Began resync as SyncSource (will sync 4193372 KB [1048343 bits set]).
[Tue Nov 17 20:39:48 2026] drbd p14: Preparing remote state change 1947659335: 1->2 role( Secondary ) conn( Connected )
[Tue Nov 17 20:39:49 2026] drbd p14 lima-dv-2: Committing remote state change 1947659335 (primary_nodes=1)
[Tue Nov 17 20:39:50 2026] drbd p14: Preparing remote state change 2943788509: 1->0 conn( Disconnecting ) disk( Outdated )
[Tue Nov 17 20:39:50 2026] drbd p14 lima-dv-2: Committing remote state change 2943788509 (primary_nodes=1)
[Tue Nov 17 20:39:50 2026] drbd p14 lima-dv-2: conn( Connected -> TearDown ) peer( Secondary -> Unknown ) [remote]
[Tue Nov 17 20:39:50 2026] drbd p14/0 drbd1014 lima-dv-2: pdsk( UpToDate -> Outdated ) repl( Established -> Off ) [remote]
[Tue Nov 17 20:39:50 2026] drbd p14/0 drbd1014: gen-obligation: DISCHARGED(promoted) -> ARMED(peer-data-lost)
[Tue Nov 17 20:39:50 2026] drbd p14 lima-dv-2: Terminating sender thread
[Tue Nov 17 20:39:50 2026] drbd p14 lima-dv-2: Starting sender thread (peer-node-id 1)
[Tue Nov 17 20:39:50 2026] drbd p14/0 drbd1014: gen-obligation: ARMED(peer-data-lost) -> MINTING(peer-data-lost)
[Tue Nov 17 20:39:50 2026] drbd p14/0 drbd1014: new current UUID: 2F3F4B60C4F7A117 weak: FFFFFFFFFFFFFFFA
[Tue Nov 17 20:39:50 2026] drbd p14/0 drbd1014: gen-obligation: MINTING(peer-data-lost) -> DISCHARGED(peer-data-lost)
[Tue Nov 17 20:39:50 2026] drbd p14 lima-dv-2: Connection closed
[Tue Nov 17 20:39:50 2026] drbd p14 lima-dv-2: conn( TearDown -> Unconnected ) [disconnected]
[Tue Nov 17 20:39:50 2026] drbd p14 lima-dv-2: Restarting receiver thread
[Tue Nov 17 20:39:50 2026] drbd p14 lima-dv-2: conn( Unconnected -> Connecting ) [connecting]
[Tue Nov 17 20:39:51 2026] drbd p14: Preparing remote state change 1912131793: 1->2 conn( Disconnecting )
[Tue Nov 17 20:39:51 2026] drbd p14 lima-dv-3: Committing remote state change 1912131793 (primary_nodes=1)
[Tue Nov 17 20:40:02 2026] drbd p14/0 drbd1014: md_sync_timer expired! Worker calls drbd_md_sync().
[Tue Nov 17 20:40:16 2026] drbd p14 lima-dv-2: Handshake to peer 1 successful: Agreed network protocol version 125
[Tue Nov 17 20:40:16 2026] drbd p14 lima-dv-2: Feature flags enabled on protocol level: 0x7ff TRIM THIN_RESYNC WRITE_SAME WRITE_ZEROES RESYNC_DAGTAG WRITE_POSTPONE
[Tue Nov 17 20:40:16 2026] drbd p14: Preparing cluster-wide state change 1812084286: 0->1 role( Primary ) conn( Connected )
[Tue Nov 17 20:40:16 2026] drbd p14/0 drbd1014 lima-dv-2: drbd_sync_handshake:
[Tue Nov 17 20:40:16 2026] drbd p14/0 drbd1014 lima-dv-2: self 2F3F4B60C4F7A117:9A4210003A0A9AA4:0000000000000000:0000000000000000 bits:16419 flags:2120
[Tue Nov 17 20:40:16 2026] drbd p14/0 drbd1014 lima-dv-2: peer 9A4210003A0A9AA4:0000000000000000:0000000000000000:0000000000000000 bits:0 flags:20
[Tue Nov 17 20:40:16 2026] drbd p14/0 drbd1014 lima-dv-2: uuid_compare()=source-use-bitmap by rule=bitmap-self
[Tue Nov 17 20:40:16 2026] drbd p14: State change 1812084286: primary_nodes=1, weak_nodes=FFFFFFFFFFFFFFF8
[Tue Nov 17 20:40:16 2026] drbd p14: Committing cluster-wide state change 1812084286 (16ms)
[Tue Nov 17 20:40:16 2026] drbd p14 lima-dv-2: conn( Connecting -> Connected ) peer( Unknown -> Secondary ) [connected]
[Tue Nov 17 20:40:16 2026] drbd p14/0 drbd1014 lima-dv-2: repl( Off -> WFBitMapS ) [connected]
[Tue Nov 17 20:40:16 2026] drbd p14/0 drbd1014 lima-dv-2: send bitmap stats [Bytes(packets)]: plain 0(0), RLE 47(1), total 47; compression: 100.0%
[Tue Nov 17 20:40:16 2026] drbd p14/0 drbd1014 lima-dv-2: receive bitmap stats [Bytes(packets)]: plain 0(0), RLE 47(1), total 47; compression: 100.0%
[Tue Nov 17 20:40:16 2026] drbd p14/0 drbd1014 lima-dv-2: pdsk( Outdated -> Inconsistent ) repl( WFBitMapS -> SyncSource ) replication( yes -> no ) [receive-bitmap]
[Tue Nov 17 20:40:16 2026] drbd p14/0 drbd1014 lima-dv-2: Began resync as SyncSource (will sync 65676 KB [16419 bits set]).
[Tue Nov 17 20:40:16 2026] drbd p14: Preparing remote state change 1595628039: 1->2 role( Secondary ) conn( Connected )
[Tue Nov 17 20:40:16 2026] drbd p14 lima-dv-2: Committing remote state change 1595628039 (primary_nodes=1)
[Tue Nov 17 20:40:23 2026] drbd p14/0 drbd1014 lima-dv-2: replication( no -> yes ) [enable-replication]
[Tue Nov 17 20:41:13 2026] drbd p14/0 drbd1014 lima-dv-2: updated UUIDs 2F3F4B60C4F7A117:0000000000000000:9A4210003A0A9AA4:0000000000000000
[Tue Nov 17 20:41:13 2026] drbd p14/0 drbd1014 lima-dv-2: Resync done (total 56 sec; paused 0 sec; 1816 K/sec)
[Tue Nov 17 20:41:13 2026] drbd p14/0 drbd1014 lima-dv-2: pdsk( Inconsistent -> UpToDate ) repl( SyncSource -> Established ) [resync-finished]
[Tue Nov 17 20:42:51 2026] drbd p14/0 drbd1014 lima-dv-3: replication( no -> yes ) [enable-replication]
[Tue Nov 17 20:43:11 2026] drbd p14/0 drbd1014 lima-dv-3: updated UUIDs 2F3F4B60C4F7A117:0000000000000000:9A4210003A0A9AA4:0000000000000000
[Tue Nov 17 20:43:11 2026] drbd p14/0 drbd1014 lima-dv-3: Resync done (total 203 sec; paused 0 sec; 21124 K/sec)
[Tue Nov 17 20:43:11 2026] drbd p14/0 drbd1014 lima-dv-3: pdsk( Inconsistent -> UpToDate ) repl( SyncSource -> Established ) [resync-finished]
[Tue Nov 17 20:43:21 2026] drbd p14: Preparing cluster-wide state change 1700696975: 0->all role( Secondary )
[Tue Nov 17 20:43:21 2026] drbd p14: State change 1700696975: primary_nodes=0, weak_nodes=0
[Tue Nov 17 20:43:21 2026] drbd p14: Committing cluster-wide state change 1700696975 (4ms)
[Tue Nov 17 20:43:21 2026] drbd p14: role( Primary -> Secondary ) [umount:1340465 auto-demote]
[Tue Nov 17 20:43:21 2026] drbd p14/0 drbd1014 lima-dv-3: pdsk( UpToDate -> Outdated ) [peer-state]
[Tue Nov 17 20:43:21 2026] drbd p14/0 drbd1014 lima-dv-3: pdsk( Outdated -> Inconsistent ) resync-susp( no -> peer ) replication( yes -> no ) [peer-state]

### P14 variant without a witness, run 2
$ [dv-1] drbdadm create-md --force --max-peers=7 p14
initializing bitmap (896 KB) to all zero
initializing activity log
Writing meta data...
New drbd meta data block successfully created.
rc=0
$ [dv-2] drbdadm create-md --force --max-peers=7 p14
initializing bitmap (896 KB) to all zero
initializing activity log
Writing meta data...
New drbd meta data block successfully created.
rc=0
$ [dv-1] drbdadm up p14
rc=0
$ [dv-2] drbdadm up p14
rc=0
wait: dv-1: 1 connections Connected after 1 s
wait: dv-2: 1 connections Connected after 1 s
$ [dv-1] drbdadm new-current-uuid --clear-bitmap p14/0
rc=0
$ [dv-1] drbdsetup status p14 --verbose
p14 node-id:0 role:Secondary suspended:no force-io-failures:no
  volume:0 minor:1014 disk:UpToDate backing_dev:/dev/ambar-exp/p14 quorum:yes
      open:no blocked:no
  lima-dv-2 node-id:1 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no

rc=0
$ [dv-1] dd if=/dev/urandom of=/dev/drbd1014 bs=1M count=2048 oflag=direct status=none
rc=0
$ [dv-1] mkfs.ext4 -q -F -E nodiscard /dev/drbd1014
rc=0
20:44:34.966
2136
started
free space on dv-1 after the 2 GiB write: /dev/vda1        20G  4.4G   15G  24% /

### P14: dv-3 joins with a copy
$ [dv-3] drbdadm create-md --force --max-peers=7 p14
initializing bitmap (896 KB) to all zero
initializing activity log
Writing meta data...
New drbd meta data block successfully created.
rc=0
$ [dv-3] drbdadm up p14
rc=0
@ 20:44:39.558 up on dv-3 (UP_T0)
$ [dv-1] drbdadm adjust p14
rc=0
$ [dv-2] drbdadm adjust p14
rc=0

### P14: wait for dv-3 Inconsistent, then down on dv-2
p14 node-id:0 role:Primary suspended:no force-io-failures:no
    write-ordering:flush
  volume:0 minor:1014 disk:UpToDate backing_dev:/dev/ambar-exp/p14 quorum:yes
      open:yes
      size:4193372 read:45837 written:2321272 al-writes:522 bm-writes:452
      upper-pending:0 lower-pending:0 al-suspended:no blocked:no
  lima-dv-2 node-id:1 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
        received:0 sent:2190104 out-of-sync:0 pending:0 unacked:0
  lima-dv-3 node-id:2 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:SyncSource peer-disk:Inconsistent done:1.05
        resync-suspended:no
        received:0 sent:44032 out-of-sync:4149340 pending:0 unacked:0 dbdt1:31.43 eta:129
@ 20:44:44.221 dv-3 seen Inconsistent (t+5 s after up); down on dv-2
$ [dv-2] drbdadm down p14
rc=0
@ 20:44:45.478 down on dv-2 returned
$ [dv-1] drbdsetup status p14 --verbose --statistics
p14 node-id:0 role:Primary suspended:no force-io-failures:no
    write-ordering:flush
  volume:0 minor:1014 disk:UpToDate backing_dev:/dev/ambar-exp/p14 quorum:yes
      open:yes
      size:4193372 read:47629 written:2350688 al-writes:522 bm-writes:456
      upper-pending:1 lower-pending:2 al-suspended:no blocked:no
  lima-dv-2 node-id:1 connection:Connecting role:Unknown tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Off peer-disk:Outdated resync-suspended:no
        received:0 sent:0 out-of-sync:15404 pending:0 unacked:0
  lima-dv-3 node-id:2 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:SyncSource peer-disk:Inconsistent done:1.09
        resync-suspended:no
        received:0 sent:45824 out-of-sync:4147548 pending:0 unacked:1 dbdt1:12.38 eta:327

rc=0
@ 20:45:06.510 20 s after down on dv-2
$ [dv-1] drbdsetup status p14 --verbose --statistics
p14 node-id:0 role:Primary suspended:no force-io-failures:no
    write-ordering:flush
  volume:0 minor:1014 disk:UpToDate backing_dev:/dev/ambar-exp/p14 quorum:yes
      open:yes
      size:4193372 read:93773 written:2730788 al-writes:522 bm-writes:468
      upper-pending:0 lower-pending:1 al-suspended:no blocked:no
  lima-dv-2 node-id:1 connection:Connecting role:Unknown tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Off peer-disk:Outdated resync-suspended:no
        received:0 sent:0 out-of-sync:65696 pending:0 unacked:0
  lima-dv-3 node-id:2 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:SyncSource peer-disk:Inconsistent done:2.19
        resync-suspended:no
        received:0 sent:91968 out-of-sync:4101416 pending:0 unacked:1 dbdt1:0.44 eta:9003

rc=0
$ [dv-3] drbdsetup status p14 --verbose --statistics
p14 node-id:2 role:Secondary suspended:no force-io-failures:no
    write-ordering:flush
  volume:0 minor:1014 disk:Inconsistent backing_dev:/dev/ambar-exp/p14 quorum:yes
      open:no
      size:4193372 read:0 written:92072 al-writes:0 bm-writes:244 upper-pending:0
      lower-pending:0 al-suspended:no blocked:no
  lima-dv-1 node-id:0 connection:Connected role:Primary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:SyncTarget peer-disk:UpToDate done:2.20 resync-suspended:no
        received:92072 sent:0 out-of-sync:4101312 pending:5 unacked:0 dbdt1:0.38
        eta:10648
  lima-dv-2 node-id:1 connection:Connecting role:Unknown tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Off peer-disk:Outdated resync-suspended:dependency
        received:0 sent:0 out-of-sync:4193372 pending:0 unacked:0

rc=0

### P14: dv-2 returns
$ [dv-2] drbdadm up p14
rc=0
@ 20:45:09.427 up on dv-2
wait: dv-2 UpToDate after 71 s
wait: dv-3 UpToDate after 84 s
@ 20:47:47.974 all copies UpToDate
$ [dv-1] drbdsetup status p14 --verbose --statistics
p14 node-id:0 role:Primary suspended:no force-io-failures:no
    write-ordering:flush
  volume:0 minor:1014 disk:UpToDate backing_dev:/dev/ambar-exp/p14 quorum:yes
      open:yes
      size:4193372 read:4329149 written:3499000 al-writes:522 bm-writes:806
      upper-pending:0 lower-pending:0 al-suspended:no blocked:no
  lima-dv-2 node-id:1 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
        received:0 sent:669668 out-of-sync:0 pending:0 unacked:0
  lima-dv-3 node-id:2 connection:Connected role:Secondary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
        received:0 sent:4271476 out-of-sync:0 pending:0 unacked:0

rc=0
$ [dv-2] drbdsetup status p14 --verbose --statistics
p14 node-id:1 role:Secondary suspended:no force-io-failures:no
    write-ordering:flush
  volume:0 minor:1014 disk:UpToDate backing_dev:/dev/ambar-exp/p14 quorum:yes
      open:no
      size:4193372 read:0 written:672560 al-writes:18 bm-writes:321 upper-pending:0
      lower-pending:0 al-suspended:no blocked:no
  lima-dv-1 node-id:0 connection:Connected role:Primary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
        received:672560 sent:0 out-of-sync:0 pending:0 unacked:2
  lima-dv-3 node-id:2 connection:StandAlone role:Unknown tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Off peer-disk:Outdated resync-suspended:no
        received:0 sent:0 out-of-sync:91212 pending:0 unacked:0

rc=0
$ [dv-3] drbdsetup status p14 --verbose --statistics
p14 node-id:2 role:Secondary suspended:no force-io-failures:no
    write-ordering:flush
  volume:0 minor:1014 disk:UpToDate backing_dev:/dev/ambar-exp/p14 quorum:yes
      open:no
      size:4193372 read:0 written:4277076 al-writes:6 bm-writes:618 upper-pending:0
      lower-pending:0 al-suspended:no blocked:no
  lima-dv-1 node-id:0 connection:Connected role:Primary tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Established peer-disk:UpToDate resync-suspended:no
        received:4277076 sent:0 out-of-sync:0 pending:0 unacked:0
  lima-dv-2 node-id:1 connection:StandAlone role:Unknown tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Off peer-disk:Outdated resync-suspended:no
        received:0 sent:0 out-of-sync:91212 pending:0 unacked:0

rc=0
@ 20:47:53.025 stop the writer
acked=79475 on-volume=79476
--- probe.sh log (stdout and stderr)
--- watch.sh log
samples=395 probe=alive:R=30 probe=alive:S=365 longest-gap-without-new-acked=0.6s (from 20:47:25.727)
--- quorum log of dv-1 (runs of equal values: first - last  value  samples)
20:44:36.393 - 20:47:54.135  "quorum": true  x197
--- dmesg -T | grep 'drbd p14' | tail -n 80 [dv-1]
[Tue Nov 17 20:44:27 2026] drbd p14/0 drbd1014: gen-obligation: MINTING(promoted) -> DISCHARGED(promoted)
[Tue Nov 17 20:44:27 2026] drbd p14: Preparing cluster-wide state change 262455620: 0->all role( Secondary )
[Tue Nov 17 20:44:27 2026] drbd p14: State change 262455620: primary_nodes=0, weak_nodes=0
[Tue Nov 17 20:44:27 2026] drbd p14: Committing cluster-wide state change 262455620 (0ms)
[Tue Nov 17 20:44:27 2026] drbd p14: role( Primary -> Secondary ) [mkfs.ext4:1344574 auto-demote]
[Tue Nov 17 20:44:27 2026] drbd p14: Preparing cluster-wide state change 2235419928: 0->all role( Primary )
[Tue Nov 17 20:44:27 2026] drbd p14: State change 2235419928: primary_nodes=1, weak_nodes=FFFFFFFFFFFFFFFC
[Tue Nov 17 20:44:27 2026] drbd p14: Committing cluster-wide state change 2235419928 (0ms)
[Tue Nov 17 20:44:27 2026] drbd p14: role( Secondary -> Primary ) [mount:1344600 auto-promote]
[Tue Nov 17 20:44:27 2026] drbd p14/0 drbd1014: gen-obligation: DISCHARGED(promoted) -> ARMED(promoted)
[Tue Nov 17 20:44:27 2026] drbd p14/0 drbd1014: gen-obligation: ARMED(promoted) -> MINTING(promoted)
[Tue Nov 17 20:44:27 2026] drbd p14/0 drbd1014: gen-obligation: MINTING(promoted) -> DISCHARGED(promoted)
[Tue Nov 17 20:44:39 2026] drbd p14 lima-dv-3: Starting sender thread (peer-node-id 2)
[Tue Nov 17 20:44:39 2026] drbd p14 lima-dv-3: conn( StandAlone -> Unconnected ) [connect]
[Tue Nov 17 20:44:39 2026] drbd p14 lima-dv-3: Starting receiver thread (peer-node-id 2)
[Tue Nov 17 20:44:39 2026] drbd p14 lima-dv-3: conn( Unconnected -> Connecting ) [connecting]
[Tue Nov 17 20:44:40 2026] drbd p14 lima-dv-3: Handshake to peer 2 successful: Agreed network protocol version 125
[Tue Nov 17 20:44:40 2026] drbd p14 lima-dv-3: Feature flags enabled on protocol level: 0x7ff TRIM THIN_RESYNC WRITE_SAME WRITE_ZEROES RESYNC_DAGTAG WRITE_POSTPONE
[Tue Nov 17 20:44:40 2026] drbd p14: Preparing cluster-wide state change 2833258330: 0->2 role( Primary ) conn( Connected )
[Tue Nov 17 20:44:40 2026] drbd p14/0 drbd1014 lima-dv-3: drbd_sync_handshake:
[Tue Nov 17 20:44:40 2026] drbd p14/0 drbd1014 lima-dv-3: self FC722A84D21B340A:A523D67895C262A6:0000000000000000:0000000000000000 bits:532511 flags:20
[Tue Nov 17 20:44:40 2026] drbd p14/0 drbd1014 lima-dv-3: peer 0000000000000004:0000000000000000:0000000000000000:0000000000000000 bits:1048343 flags:2024
[Tue Nov 17 20:44:40 2026] drbd p14/0 drbd1014 lima-dv-3: uuid_compare()=source-set-bitmap by rule=just-created-peer
[Tue Nov 17 20:44:40 2026] drbd p14/0 drbd1014 lima-dv-3: Setting and writing one bitmap slot, after drbd_sync_handshake
[Tue Nov 17 20:44:40 2026] drbd p14: State change 2833258330: primary_nodes=1, weak_nodes=FFFFFFFFFFFFFFF8
[Tue Nov 17 20:44:40 2026] drbd p14: Committing cluster-wide state change 2833258330 (20ms)
[Tue Nov 17 20:44:40 2026] drbd p14 lima-dv-3: conn( Connecting -> Connected ) peer( Unknown -> Secondary ) [connected]
[Tue Nov 17 20:44:40 2026] drbd p14/0 drbd1014 lima-dv-3: pdsk( DUnknown -> Inconsistent ) repl( Off -> WFBitMapS ) replication( yes -> no ) [connected]
[Tue Nov 17 20:44:40 2026] drbd p14/0 drbd1014 lima-dv-3: send bitmap stats [Bytes(packets)]: plain 0(0), RLE 21(1), total 21; compression: 100.0%
[Tue Nov 17 20:44:40 2026] drbd p14/0 drbd1014 lima-dv-3: receive bitmap stats [Bytes(packets)]: plain 0(0), RLE 21(1), total 21; compression: 100.0%
[Tue Nov 17 20:44:40 2026] drbd p14/0 drbd1014 lima-dv-3: repl( WFBitMapS -> SyncSource ) [receive-bitmap]
[Tue Nov 17 20:44:40 2026] drbd p14/0 drbd1014 lima-dv-3: Began resync as SyncSource (will sync 4193372 KB [1048343 bits set]).
[Tue Nov 17 20:44:41 2026] drbd p14: Preparing remote state change 2104196946: 1->2 role( Secondary ) conn( Connected )
[Tue Nov 17 20:44:41 2026] drbd p14 lima-dv-2: Committing remote state change 2104196946 (primary_nodes=1)
[Tue Nov 17 20:44:42 2026] drbd p14: Preparing remote state change 1406889565: 1->0 conn( Disconnecting ) disk( Outdated )
[Tue Nov 17 20:44:42 2026] drbd p14 lima-dv-2: Committing remote state change 1406889565 (primary_nodes=1)
[Tue Nov 17 20:44:42 2026] drbd p14 lima-dv-2: conn( Connected -> TearDown ) peer( Secondary -> Unknown ) [remote]
[Tue Nov 17 20:44:42 2026] drbd p14/0 drbd1014 lima-dv-2: pdsk( UpToDate -> Outdated ) repl( Established -> Off ) [remote]
[Tue Nov 17 20:44:42 2026] drbd p14/0 drbd1014: gen-obligation: DISCHARGED(promoted) -> ARMED(peer-data-lost)
[Tue Nov 17 20:44:42 2026] drbd p14 lima-dv-2: Terminating sender thread
[Tue Nov 17 20:44:42 2026] drbd p14 lima-dv-2: Starting sender thread (peer-node-id 1)
[Tue Nov 17 20:44:42 2026] drbd p14/0 drbd1014: gen-obligation: ARMED(peer-data-lost) -> MINTING(peer-data-lost)
[Tue Nov 17 20:44:42 2026] drbd p14/0 drbd1014: new current UUID: CD284E691A7D3A19 weak: FFFFFFFFFFFFFFFA
[Tue Nov 17 20:44:42 2026] drbd p14/0 drbd1014: gen-obligation: MINTING(peer-data-lost) -> DISCHARGED(peer-data-lost)
[Tue Nov 17 20:44:42 2026] drbd p14 lima-dv-2: Connection closed
[Tue Nov 17 20:44:42 2026] drbd p14 lima-dv-2: conn( TearDown -> Unconnected ) [disconnected]
[Tue Nov 17 20:44:42 2026] drbd p14 lima-dv-2: Restarting receiver thread
[Tue Nov 17 20:44:42 2026] drbd p14 lima-dv-2: conn( Unconnected -> Connecting ) [connecting]
[Tue Nov 17 20:44:43 2026] drbd p14: Preparing remote state change 3556844296: 1->2 conn( Disconnecting )
[Tue Nov 17 20:44:43 2026] drbd p14 lima-dv-3: Committing remote state change 3556844296 (primary_nodes=1)
[Tue Nov 17 20:44:57 2026] drbd p14/0 drbd1014: md_sync_timer expired! Worker calls drbd_md_sync().
[Tue Nov 17 20:45:07 2026] drbd p14 lima-dv-2: Handshake to peer 1 successful: Agreed network protocol version 125
[Tue Nov 17 20:45:07 2026] drbd p14 lima-dv-2: Feature flags enabled on protocol level: 0x7ff TRIM THIN_RESYNC WRITE_SAME WRITE_ZEROES RESYNC_DAGTAG WRITE_POSTPONE
[Tue Nov 17 20:45:07 2026] drbd p14: Preparing cluster-wide state change 2689707577: 0->1 role( Primary ) conn( Connected )
[Tue Nov 17 20:45:07 2026] drbd p14/0 drbd1014 lima-dv-2: drbd_sync_handshake:
[Tue Nov 17 20:45:07 2026] drbd p14/0 drbd1014 lima-dv-2: self CD284E691A7D3A19:FC722A84D21B340A:0000000000000000:0000000000000000 bits:16428 flags:2120
[Tue Nov 17 20:45:07 2026] drbd p14/0 drbd1014 lima-dv-2: peer FC722A84D21B340A:0000000000000000:0000000000000000:0000000000000000 bits:0 flags:20
[Tue Nov 17 20:45:07 2026] drbd p14/0 drbd1014 lima-dv-2: uuid_compare()=source-use-bitmap by rule=bitmap-self
[Tue Nov 17 20:45:07 2026] drbd p14: State change 2689707577: primary_nodes=1, weak_nodes=FFFFFFFFFFFFFFF8
[Tue Nov 17 20:45:07 2026] drbd p14: Committing cluster-wide state change 2689707577 (16ms)
[Tue Nov 17 20:45:07 2026] drbd p14 lima-dv-2: conn( Connecting -> Connected ) peer( Unknown -> Secondary ) [connected]
[Tue Nov 17 20:45:07 2026] drbd p14/0 drbd1014 lima-dv-2: repl( Off -> WFBitMapS ) [connected]
[Tue Nov 17 20:45:07 2026] drbd p14/0 drbd1014 lima-dv-2: send bitmap stats [Bytes(packets)]: plain 0(0), RLE 47(1), total 47; compression: 100.0%
[Tue Nov 17 20:45:07 2026] drbd p14: Preparing remote state change 2961952057: 1->2 role( Secondary ) conn( Connected )
[Tue Nov 17 20:45:07 2026] drbd p14/0 drbd1014 lima-dv-2: receive bitmap stats [Bytes(packets)]: plain 0(0), RLE 47(1), total 47; compression: 100.0%
[Tue Nov 17 20:45:07 2026] drbd p14/0 drbd1014 lima-dv-2: pdsk( Outdated -> Inconsistent ) repl( WFBitMapS -> SyncSource ) replication( yes -> no ) [receive-bitmap]
[Tue Nov 17 20:45:07 2026] drbd p14/0 drbd1014 lima-dv-2: Began resync as SyncSource (will sync 65712 KB [16428 bits set]).
[Tue Nov 17 20:45:07 2026] drbd p14 lima-dv-2: Aborting remote state change 2961952057
[Tue Nov 17 20:45:15 2026] drbd p14/0 drbd1014 lima-dv-2: replication( no -> yes ) [enable-replication]
[Tue Nov 17 20:46:16 2026] drbd p14/0 drbd1014 lima-dv-2: updated UUIDs CD284E691A7D3A19:0000000000000000:FC722A84D21B340A:0000000000000000
[Tue Nov 17 20:46:16 2026] drbd p14/0 drbd1014 lima-dv-2: Resync done (total 68 sec; paused 0 sec; 1536 K/sec)
[Tue Nov 17 20:46:16 2026] drbd p14/0 drbd1014 lima-dv-2: pdsk( Inconsistent -> UpToDate ) repl( SyncSource -> Established ) [resync-finished]
[Tue Nov 17 20:47:36 2026] drbd p14/0 drbd1014 lima-dv-3: replication( no -> yes ) [enable-replication]
[Tue Nov 17 20:47:41 2026] drbd p14/0 drbd1014 lima-dv-3: updated UUIDs CD284E691A7D3A19:0000000000000000:FC722A84D21B340A:0000000000000000
[Tue Nov 17 20:47:41 2026] drbd p14/0 drbd1014 lima-dv-3: Resync done (total 181 sec; paused 0 sec; 23544 K/sec)
[Tue Nov 17 20:47:41 2026] drbd p14/0 drbd1014 lima-dv-3: pdsk( Inconsistent -> UpToDate ) repl( SyncSource -> Established ) [resync-finished]
[Tue Nov 17 20:47:52 2026] drbd p14: Preparing cluster-wide state change 3088165589: 0->all role( Secondary )
[Tue Nov 17 20:47:52 2026] drbd p14: State change 3088165589: primary_nodes=0, weak_nodes=0
[Tue Nov 17 20:47:52 2026] drbd p14: Committing cluster-wide state change 3088165589 (4ms)
[Tue Nov 17 20:47:52 2026] drbd p14: role( Primary -> Secondary ) [umount:1434485 auto-demote]
exit=0
```


## Проверки агента на стенде

Первые две проверки выполнены 2026-11-13 на стенде в поде `toolbox`, по одному прогону. Третья
и четвёртая строки таблицы взяты из прогонов `stand/agent-smoke.sh` того же дня.

| Что проверялось | Команда | Что наблюдалось |
|---|---|---|
| Вывод `losetup` для одного устройства | `losetup --list --noheadings --output BACK-FILE <dev>` | Печатает путь к файлу и перевод строки. После удаления файла печатает `<путь> (deleted)`. Для отсоединённого устройства печатает пустую строку, код выхода 0. Сразу после `losetup -d` та же команда ещё раз напечатала старый путь; `losetup -a` через короткое время был пуст: отсоединение не мгновенное |
| Узел с копией становится узлом без диска | В секции узла файла ресурса указан `disk none`, затем `drbdadm adjust <res>` только на этом узле | Код выхода 0. В течение 3 с статус узла: `disk-state: Diskless`, `client: true`, обе связи `Connected`. Соседи показали `peer-disk:Diskless peer-client:yes` |
| Лидер и узел без диска в `ambar apply` | `stand/agent-smoke.sh`, шаг c | Первый `ambar apply` на `dv-1` после применения на трёх узлах напечатал `[SkipInitialSync]` (прошло 1 с с начала опроса, включая сам вызов). Узел без диска при первом применении печатает `[WriteRes, Up, MarkApplied]`; `ambar remove` на нём печатает `["down", "remove-res", "remove-applied"]`, шага `remove-link` нет: ссылку `/dev/ambar/<vol>` создают только узлы с копией |
| Узел с копией после принудительной остановки ВМ | `stand/agent-smoke.sh --reboot`, шаг k: `limactl stop --force dv-2`, `stand/up.sh`, затем в поде `toolbox` на `dv-2` `ambar load-module` и `ambar apply` | `ambar load-module` напечатал строку, начинающуюся с `loaded drbd 9.3.4` (скрипт сверяет эту часть). `ambar apply` напечатал `[AttachLoop, LinkDevice, Up, MarkApplied]`; диск `dv-2` в `ambar status` стал `UpToDate` (скрипт ждёт до 120 с). `dv-1` не останавливался. Один узел |

Ограничения: первые две проверки выполнены по одному разу. `stand/agent-smoke.sh` 2026-11-13 запускался несколько раз: дважды автором, один раз при приёмке, затем ещё раз без `--reboot` и раз с `--reboot` после правок скрипта (остатки прошлого запуска, `blockdev --getsize64`, `flock`); оба последних запуска закончились `OK: agent smoke`. Код выхода 75 проверен только при блокировке, которую держит утилита `flock` (шаг j скрипта); два одновременных `ambar apply` не запускались.

### Путь файла в списке loop-устройств после замены пода агента

Наблюдение 2026-11-14, стенд, `dv-1`: один прогон. Устройство `/dev/loop0` создал под агента, затем под удалили и DaemonSet создал новый.

| Где смотрели | `losetup --list --noheadings --output NAME,BACK-FILE` |
|---|---|
| Под, создавший устройство (и хосты `dv-2`, `dv-3`, где поды не заменялись) | `/dev/loop0 /var/lib/rancher/ambar/volumes/<vol>.img` |
| Новый под на `dv-1` и хост `dv-1` после удаления старого пода | `/dev/loop0 /<vol>.img` |

`/sys/block/loop0/loop/backing_file` на `dv-1` вернул `/<vol>.img`; `losetup -j /var/lib/rancher/ambar/volumes/<vol>.img` вернул `/dev/loop0: [65025]:266000 (/<vol>.img)`. Второе устройство для файла после замены пода не создавалось. Объяснение `[проверить: не перепроверялось]`: ядро строит путь от монтирования, через которое файл был открыт; это приватное bind-монтирование каталога томов в старом поде, после выхода пода оно отсоединено, и путь отсчитывается от корня этого монтирования.

Следствие для `backing::loop_holders`: строка вида `/<vol>.img` (в том числе `/<vol>.img (deleted)`) считается строкой этого образа; без `(deleted)` и при существующем файле устройство подтверждается по inode через `losetup -j`.

### Удаление тома после замены подов агента (2026-11-14)

Образ собран из дерева с коммитами `4d11f79` и `04f44f6` (`image/build.sh`, 19 с), установлен `stand/install.sh` (16 с, последняя строка `OK: ambar installed on 3 nodes`). Остатки прерванного прогона: PVC и под `smoke13`, том `pvc-6cc25132-…` на трёх узлах. Один прогон.

| Шаг | Что наблюдалось |
|---|---|
| `losetup --list --noheadings --output NAME,BACK-FILE` в поде агента и на хосте, после замены подов | На `dv-1`, `dv-2`, `dv-3`: `/dev/loop0 /pvc-6cc25132-….img` |
| Чтение из пода `smoke13` после замены подов | `hello-ambar`, `restartCount` 0 |
| Удаление пода и PVC `smoke13` | Объект `ReplicatedVolume` пропал через ~2 мин 17 с после удаления PVC. Журнал агента на каждом узле: `removed the local copy ["down", "detach-loop", "remove-file", "remove-marker", "remove-synced-marker", "remove-link", "remove-res", "remove-applied"]`. На всех узлах после этого `losetup --list` пуст, `volumes/` и `res/` пусты, `drbdsetup status`: `# No currently configured DRBD found.` |
| Дополнительно: PVC и под `smoke14` на том же драйвере, под агента `dv-3` заменён при живом томе | На `dv-3` устройство показано как `/dev/loop0 /pvc-3471a8ab-….img`, на `dv-2` как `/dev/loop0 /var/lib/rancher/ambar/volumes/pvc-3471a8ab-….img`; чтение `hello-ambar` после замены; после удаления пода и PVC: `detach-loop` в журнале каждого узла, `losetup --list` пуст, файлов нет, `ReplicatedVolume` исчез за ~2 мин 16 с |

Задержка между удалением PVC и исчезновением `ReplicatedVolume` не исследовалась. Шаги 4 (`stand/install.sh --remove`, `stand/check.sh`, повторная установка) и 5 (повторная проверка на переустановленном драйвере) не выполнены: запуск `--remove` отклонён системой разрешений. После прогона драйвер установлен, томов нет, `stand/check.sh` печатает `OK: stand is up`.


## Проверки CSI на стенде

Стенд 2026-11-14: четыре ВМ Lima (`dv-1`..`dv-4`, по 2 CPU, 3 ГиБ RAM, 20 ГиБ диска, шаблон `stand/lima/node.yaml`), ядро `6.12.116+deb13-arm64`, k3s `v1.37.2+k3s1`, драйвер из `deploy/` (образ `ambar-drbd:dev`, собран `image/build.sh` из дерева с коммитом `4d32d57`). Хост: macOS, 18 ГиБ RAM. Сквозную проверку выполняет `stand/csi-e2e.sh`. Образ тестовых подов: `docker.io/library/busybox@sha256:a7dbc319b1438115a207e11930dc4bb3e643dcb3ec0bdd694bb36cd8fad289ec` (индекс `busybox:1.37.0`).

### Четвёртая ВМ

| Действие | Результат |
|---|---|
| `NODES="dv-1 dv-2 dv-3 dv-4" stand/up.sh` | Создана `dv-4` из того же шаблона, ядро совпало с `KVER` без перезапуска, 2 мин 13 с, последняя строка `OK: stand is up` |
| `NODES="dv-1 dv-2 dv-3 dv-4" stand/k3s.sh` | `dv-4` присоединена командой `k3s agent` (`K3S_URL=https://<адрес dv-1>:6443`, токен первого server), 1 мин 32 с; `kc get nodes`: четыре узла `Ready`, роли `control-plane,etcd` у `dv-1`..`dv-3`, у `dv-4` роли нет |
| `stand/check.sh` с `NODES` из четырёх узлов и без переменной | В обоих случаях `OK: stand is up` (во втором проверены три узла) |
| `NODES="dv-1 dv-2 dv-3 dv-4" stand/install.sh` на установленном драйвере | 49 с; `ConfigMap ambar-nodes` с четырьмя записями (`nodeId` 0..3, адрес `dv-4` `192.168.104.5`); DaemonSet перезапущен, четыре пода агента `3/3 Running`; последняя строка `OK: ambar installed on 4 nodes` |
| `stand/csi-e2e.sh` без `NODES` | Печатает `FAIL: steps e and f need a node outside the volume…`, код возврата 2 |

Память хоста в 03:06, во время одного из прогонов до правки манифеста: `vm_stat` показывал 7417 свободных страниц по 16 КиБ, `sysctl vm.swapusage`: 1796,88 МиБ из 3072 МиБ занято. ВМ не уменьшались.

### Прогоны

Все прогоны с `NODES="dv-1 dv-2 dv-3 dv-4"`.

| Прогон | Время | Итог |
|---|---|---|
| 3 прогона при отладке скрипта | 1 мин 9 с, 56 с, 1 мин 37 с | Остановились на ошибках самого скрипта (срабатывание `trap ERR` внутри `eval`, события `FailedMount` от пода `dup3` прежнего прогона, слишком короткое окно замеров в шаге h). Скрипт исправлен, результаты не используются |
| 2 прогона после исправлений, манифест `deploy/daemonset.yaml` без правки | 2 мин 56 с, 2 мин 54 с | Оба `OK: csi e2e` |
| 3-й и 4-й прогоны (после дополнений скрипта) | — | Прогон 3: ошибка шага k, `kubectl rollout status ds/ambar --timeout=240s` не дождался (под агента на `dv-2` в `ContainerCreating` ~100 с). Прогон 4: ошибка в начальной очистке, таймаут того же `rollout status`. Причина — см. «Удвоение монтирований» ниже |
| **Прогон 1 из 2 после правки манифеста** | 3 мин 11 с | `OK: csi e2e` |
| **Прогон 2 из 2 после правки манифеста** | 3 мин 02 с | `OK: csi e2e` |

Значения в таблице шагов ниже — из двух последних прогонов (подряд, после правки манифеста).

### Шаги и наблюдавшиеся значения

«Время» — секунды от действия, `wait_until` опрашивает раз в 2 с, поэтому значение 0 означает «выполнено при первой проверке».

| Шаг | Что проверено | Прогон 1 | Прогон 2 |
|---|---|---|---|
| a | PVC `e2e-r3` (`ambar-r3`) `Bound`, под `w3` на `dv-1`, три `Replica`, `initialSync: Done`, три записи `status.nodes` `UpToDate` | `Bound` через 10 с; под `Ready` на 25-й с прогона; записи `UpToDate` сразу после `Ready` | `Bound` через 3 с; под `Ready` на 16-й с; записи сразу |
| b | `blockdev --getsize64 /dev/drbd1000` на `dv-1` | 1073766400 | 1073766400 |
| c | Под `r3` на `dv-2`, файл `/data/f` совпал, на `dv-1` роль `Secondary` | `r3` `Ready` на 32-й с; `Secondary` на `dv-1` при первой проверке (1 с) | 22-я с; 1 с |
| d | PVC `e2e-r2` (`ambar-r2`), под на `dv-1`: 2 `Replica`, 1 `Witness`, 3 записи в `spec.nodes` | `Bound` через 29 с; `dv-1`, `dv-4` Replica, `dv-2` Witness, вне тома `dv-3` | `Bound` через 30 с; тот же состав |
| e | Под на узле вне тома (`dv-3`): запись `Client`, `Running`, данные совпали, файла `<том>.img` на узле нет | `Ready` на 96-й с прогона; запись `lima-dv-3=Client` | 87-я с; запись `Client` |
| f | Удаление пода на `dv-3` | `kubectl delete pod --wait` 1 с; запись `Client` исчезла при первой проверке; `drbdsetup status` на `dv-3`: `<том>: No such resource`; у `dv-1`, `dv-2`, `dv-4` строк с `lima-dv-3` нет | 2 с; то же |
| g | Под на `Witness` (`dv-2`) | `spec.nodes` до и после пода (и через 10 с после его удаления) совпал; запись `Witness` на месте; данные совпали | то же |
| h | `rollout restart ds/ambar` под писателем `r3` на `dv-2` (`dd conv=fsync` раз в секунду) | `rollout restart` и `rollout status` вместе 4 с; отсчёты счётчика 96 96 97 97 98 98 99 99 99; через 12 с от начала перезапуска 108; `restartCount` пода 0; испорченное поле `message` в записи `dv-2` возвращено агентом за 0 с (первая проверка) | 4 с; 96 97 97 98 98 99 99 100 100; 108; `restartCount` 0; 0 с |
| i | StorageClass `e2e-r1`, `replicas: "1"` | PVC `Pending`; событие `ProvisioningFailed` через 0 с: `rpc error: code = InvalidArgument desc = replicas is 1, at least 2 are required` | то же |
| j | Удаление подов и PVC | PVC исчезли 0 с, объекты `ReplicatedVolume` через 3 с после удаления PVC; на всех четырёх узлах файлов, loop-устройств и ресурсов DRBD нет (`# No currently configured DRBD found.`) | 1 с и 3 с; то же |
| k | Объект удалён при отсутствии агента на `dv-2` | под агента `dv-2` исчез через 0 с после метки; остался финализатор `replica-lima-dv-2` (финализаторы остальных узлов агенты сняли сами); после снятия финализатора и возврата агента (3 с) в журнале: `ambar-agent: error: volume "<том>": refused: a backing file exists but no ReplicatedVolume object has this name`; файл `<том>.img` на `dv-2` остался; `ambar remove --name <том>` в поде агента напечатал `["down", "detach-loop", "remove-file", "remove-marker", "remove-synced-marker", "remove-link", "remove-res", "remove-applied"]`; на `dv-1`, `dv-3`, `dv-4` файлов тома не было до очистки | то же, 1 с и 3 с |

Способ убрать агент с узла в шаге k: в `DaemonSet ambar` добавляется `nodeAffinity` с условием `ambar-e2e-skip DoesNotExist` (шаблон меняется, агенты перезапускаются один раз), затем узел получает метку `ambar-e2e-skip=true`; после проверки метка и `nodeAffinity` снимаются (при ошибке скрипта то же делает его обработчик).

Задержка между удалением PVC и исчезновением `ReplicatedVolume` в этих прогонах 3 с. В прогонах 2026-11-14 после замены подов агента (раздел выше) она составила около 2 мин 17 с. Причина разницы не исследовалась: `[проверить: влияние накопленных монтирований (см. ниже), состояние стенда на тот момент]`. В одном из прогонов до правки манифеста под `r3` оставался в `Terminating` ~55 с (kubelet начал размонтирование через ~34 с после выхода контейнера), в журнале агента узла с копией при этом: `the local copy is kept: peer lima-dv-2 is Primary`.

Заметка о сопоставимости: шаг i ожидает `InvalidArgument` (регистр и написание gRPC), а не `INVALID_ARGUMENT` из текста задачи; проверка без учёта регистра и подчёркивания.

### Удвоение монтирований в поде агента

Симптом: в прогонах до правки манифеста под агента на `dv-2` оставался в `ContainerCreating` ~100 с (`Created` через 78 с, `Started` через 37 с после этого), `rollout status` не укладывался в 240 с, load average ВМ 5,84.

Измерение на узлах (`/proc/self/mountinfo` хоста):

| Узел | Строк всего | Строк с `plugins/ambar` |
|---|---|---|
| `dv-1` | 8242 | 8191 |
| `dv-2` | 32800 | 32767 |
| `dv-3` | 16419 | 16383 |
| `dv-4` | 4131 | 4095 |

Числа равны 2^n − 1. В `deploy/daemonset.yaml` контейнер `ambar` монтировал `/var/lib/kubelet` с `mountPropagation: Bidirectional` и ещё раз тот же каталог сокета (`socket-dir`, hostPath `/var/lib/kubelet/plugins/ambar`) в `/var/lib/kubelet/plugins/ambar`. Объяснение: второе bind-монтирование внутри двунаправленного монтирования распространяется на хост, и каждый следующий запуск пода монтирует каталог поверх уже накопленной стопки; `[проверить: механизм не изолирован отдельным опытом]`.

Исправление: из контейнера `ambar` убрано монтирование `socket-dir` (каталог входит в монтирование `/var/lib/kubelet`; сайдкары монтируют `socket-dir` в `/csi` без распространения). Накопленные монтирования сняты на всех узлах командой `umount -R -l /var/lib/kubelet/plugins/ambar` (на `dv-2` команда шла несколько минут). После правки три подряд `rollout restart ds/ambar`: `/proc/self/mountinfo` хоста без изменений (`dv-1` 51, `dv-2` 36, `dv-3` 36, `dv-4` 37 строк; строк с `plugins/ambar` 0, на `dv-4` 1). Одно монтирование на `dv-4` не растёт: в `/proc/self/mountinfo` это одна строка `393 31 254:1 /var/lib/kubelet/plugins/ambar /var/lib/kubelet/plugins/ambar rw,relatime shared:1 - ext4 /dev/vda1` (bind каталога на себя, `findmnt` показывает `shared`); число не менялось при `rollout restart` и при повторных запусках `stand/install.sh` и `stand/csi-e2e.sh` 2026-11-14 (на `dv-1`–`dv-3` 0). Откуда оно взялось на `dv-4`, не выяснено: `[проверить: остаток после umount -R -l или монтирование, появившееся позже]`. `stand/install.sh` предупреждает, когда на узле больше одного такого монтирования. Два последних прогона `stand/csi-e2e.sh` выполнены после этого.

### Наблюдения при живых томах

Выводы скрипта прогона 2 из 2 (имена томов и узлы как в прогоне).

**1. Узел `Client`, том смонтирован (шаг e, `dv-3`), в поде агента.**

```
blockdev --getsize64 /dev/drbd1001  ->  1073766400   (rc=0)
blkid -p -o value -s TYPE /dev/drbd1001  ->  ext4    (rc=0)
```

Минор `1001` — `spec.minor` тома `e2e-r2` (JSON в пункте 5); `1000` в таблице шага b относится к тому `e2e-r3`. Вывод скрипта печатает только результаты команд. Статус на `dv-3`:

```
pvc-c680ac7b-a9b6-4b81-a80e-7ef32ee72b9c role:Primary
  disk:Diskless client:yes open:yes
  lima-dv-1 role:Secondary
    peer-disk:UpToDate
  lima-dv-2 role:Secondary
    peer-disk:Diskless peer-client:yes
  lima-dv-4 role:Secondary
    peer-disk:UpToDate
```

**2. Роль после размонтирования.** Шаг c, `dv-1` (Replica) после удаления пода, первая проверка через 1 с:

```
pvc-49ebb6ca-352a-4899-90e8-77dc7462b09e role:Secondary
  disk:UpToDate open:no
  lima-dv-2 role:Primary
    peer-disk:UpToDate
  lima-dv-3 role:Secondary
    peer-disk:UpToDate
```

Шаг g, `dv-2` (Witness) после удаления пода:

```
pvc-c680ac7b-a9b6-4b81-a80e-7ef32ee72b9c role:Secondary
  disk:Diskless client:yes open:no
  lima-dv-1 role:Secondary
    peer-disk:UpToDate
  lima-dv-4 role:Secondary
    peer-disk:UpToDate
```

**3. Второй под с тем же PVC на другом узле, пока том смонтирован.** Том `e2e-r3` (Replica `dv-1`, `dv-2`, `dv-3`), под `r3` на `dv-2`, второй под `dup3` на `dv-4` (не член тома). Оба прогона: событие через 3 с, под `Pending`:

```
Warning   FailedMount   MountVolume.MountDevice failed for volume "pvc-49ebb6ca-352a-4899-90e8-77dc7462b09e" : rpc error: code = FailedPrecondition desc = the volume is open on another node: cannot read /dev/drbd1000 (in use on another node): Wrong medium type (os error 124)
```

`drbdsetup status` на `dv-4` в момент события: `<том>: No such resource`. На `dv-2` в прогоне 2: `lima-dv-4 connection:Connecting` (в прогоне 1: `lima-dv-4 role:Secondary`, `peer-disk:Diskless peer-client:yes`). `spec.nodes` до пода и в момент события: три `Replica`, записи `Client` для `dv-4` не было; после удаления пода тоже; `drbdsetup status` на `dv-2` через 5 с после удаления пода (прогон 2): строки `dv-4` нет. `[проверить: появлялась ли запись Client на dv-4 между созданием пода и первой проверкой; скрипт читает spec.nodes только после события]`. Роль на `dv-2` оставалась `Primary`, `open:yes`; под `r3` `Running`.

**4.** `Client` без доступной копии не создавался: нужен разрыв сети (фаза 3).

**5. `kc get rv <том> -o json --show-managed-fields`, том `e2e-r2` (Replica `dv-1`, `dv-4`, Witness `dv-2`), после `UpToDate` двух копий.**

```json
{
    "apiVersion": "ambar.balookrd.github.io/v1alpha1",
    "kind": "ReplicatedVolume",
    "metadata": {
        "annotations": {
            "ambar.balookrd.github.io/formatted": "true"
        },
        "creationTimestamp": "2026-11-13T19:26:29Z",
        "finalizers": [
            "ambar.balookrd.github.io/replica-lima-dv-1",
            "ambar.balookrd.github.io/replica-lima-dv-4"
        ],
        "generation": 2,
        "managedFields": [
            {
                "apiVersion": "ambar.balookrd.github.io/v1alpha1",
                "fieldsType": "FieldsV1",
                "fieldsV1": {
                    "f:status": {
                        "f:nodes": {
                            "k:{\"name\":\"lima-dv-2\"}": {
                                ".": {},
                                "f:connections": {},
                                "f:disk": {},
                                "f:message": {},
                                "f:name": {},
                                "f:quorum": {},
                                "f:role": {}
                            }
                        }
                    }
                },
                "manager": "ambar-lima-dv-2",
                "operation": "Apply",
                "subresource": "status",
                "time": "2026-11-13T19:26:58Z"
            },
            {
                "apiVersion": "ambar.balookrd.github.io/v1alpha1",
                "fieldsType": "FieldsV1",
                "fieldsV1": {
                    "f:status": {
                        "f:nodes": {
                            "k:{\"name\":\"lima-dv-4\"}": {
                                ".": {},
                                "f:connections": {},
                                "f:disk": {},
                                "f:message": {},
                                "f:name": {},
                                "f:quorum": {},
                                "f:role": {}
                            }
                        }
                    }
                },
                "manager": "ambar-lima-dv-4",
                "operation": "Apply",
                "subresource": "status",
                "time": "2026-11-13T19:26:58Z"
            },
            {
                "apiVersion": "ambar.balookrd.github.io/v1alpha1",
                "fieldsType": "FieldsV1",
                "fieldsV1": {
                    "f:status": {
                        "f:nodes": {
                            "k:{\"name\":\"lima-dv-1\"}": {
                                ".": {},
                                "f:connections": {},
                                "f:disk": {},
                                "f:message": {},
                                "f:name": {},
                                "f:quorum": {},
                                "f:role": {}
                            }
                        }
                    }
                },
                "manager": "ambar-lima-dv-1",
                "operation": "Apply",
                "subresource": "status",
                "time": "2026-11-13T19:27:00Z"
            },
            {
                "apiVersion": "ambar.balookrd.github.io/v1alpha1",
                "fieldsType": "FieldsV1",
                "fieldsV1": {
                    "f:metadata": {
                        "f:annotations": {
                            ".": {},
                            "f:ambar.balookrd.github.io/formatted": {}
                        },
                        "f:finalizers": {
                            ".": {},
                            "v:\"ambar.balookrd.github.io/replica-lima-dv-1\"": {},
                            "v:\"ambar.balookrd.github.io/replica-lima-dv-4\"": {}
                        }
                    },
                    "f:spec": {
                        ".": {},
                        "f:initialSync": {},
                        "f:maxPeers": {},
                        "f:minor": {},
                        "f:nodes": {
                            ".": {},
                            "k:{\"name\":\"lima-dv-1\"}": {
                                ".": {},
                                "f:address": {},
                                "f:kind": {},
                                "f:name": {},
                                "f:nodeId": {}
                            },
                            "k:{\"name\":\"lima-dv-2\"}": {
                                ".": {},
                                "f:address": {},
                                "f:kind": {},
                                "f:name": {},
                                "f:nodeId": {}
                            },
                            "k:{\"name\":\"lima-dv-4\"}": {
                                ".": {},
                                "f:address": {},
                                "f:kind": {},
                                "f:name": {},
                                "f:nodeId": {}
                            }
                        },
                        "f:port": {},
                        "f:protocol": {},
                        "f:sizeBytes": {}
                    }
                },
                "manager": "unknown",
                "operation": "Update",
                "time": "2026-11-13T19:27:00Z"
            }
        ],
        "name": "pvc-c680ac7b-a9b6-4b81-a80e-7ef32ee72b9c",
        "resourceVersion": "278049",
        "uid": "e72c7189-a939-4eb2-8d5f-bdf7074fe7b4"
    },
    "spec": {
        "initialSync": "Done",
        "maxPeers": 7,
        "minor": 1001,
        "nodes": [
            {
                "address": "192.168.104.1",
                "kind": "Replica",
                "name": "lima-dv-1",
                "nodeId": 0
            },
            {
                "address": "192.168.104.3",
                "kind": "Witness",
                "name": "lima-dv-2",
                "nodeId": 1
            },
            {
                "address": "192.168.104.5",
                "kind": "Replica",
                "name": "lima-dv-4",
                "nodeId": 3
            }
        ],
        "port": 7001,
        "protocol": "C",
        "sizeBytes": 1073741824
    },
    "status": {
        "nodes": [
            {
                "connections": [
                    {
                        "peer": "lima-dv-1",
                        "peerDisk": "UpToDate",
                        "state": "Connected"
                    },
                    {
                        "peer": "lima-dv-4",
                        "peerDisk": "UpToDate",
                        "state": "Connected"
                    }
                ],
                "disk": "Diskless",
                "message": "",
                "name": "lima-dv-2",
                "quorum": true,
                "role": "Secondary"
            },
            {
                "connections": [
                    {
                        "peer": "lima-dv-1",
                        "peerDisk": "UpToDate",
                        "state": "Connected"
                    },
                    {
                        "peer": "lima-dv-2",
                        "peerDisk": "Diskless",
                        "state": "Connected"
                    }
                ],
                "disk": "UpToDate",
                "message": "",
                "name": "lima-dv-4",
                "quorum": true,
                "role": "Secondary"
            },
            {
                "connections": [
                    {
                        "peer": "lima-dv-2",
                        "peerDisk": "Diskless",
                        "state": "Connected"
                    },
                    {
                        "peer": "lima-dv-4",
                        "peerDisk": "UpToDate",
                        "state": "Connected"
                    }
                ],
                "disk": "UpToDate",
                "message": "",
                "name": "lima-dv-1",
                "quorum": true,
                "role": "Primary"
            }
        ]
    }
}
```

### Не выполнено

| Что | Причина |
|---|---|
| `stand/install.sh --remove` и `stand/agent-smoke.sh` (шаг 5 задачи) | Удаление CRD и пространства имён ждёт решения владельца репозитория; не запускалось |
| Бездисковый `Client` без доступной копии | Нужен разрыв сети; фаза 3 |
| Ветка установки ядра в `stand/up.sh` | Не выполнялась: ядро на `dv-4` совпало с `KVER` после создания ВМ |

## Повторная проверка CSI после второго раунда исправлений

Стенд 2026-11-14: те же четыре ВМ (`dv-1`..`dv-4`), драйвер из `deploy/`, образ `ambar-drbd:dev`, собранный `image/build.sh` из дерева с коммитом `f26563d` (правила: снятие членства `Client` по состоянию узла, `Open { in_use: true }` отдаётся как `UNAVAILABLE`, повтор через 5 с для копий, удерживаемых из-за состояния DRBD). Перед прогонами выполнены `image/build.sh` и `NODES="dv-1 dv-2 dv-3 dv-4" stand/install.sh` (16 с, последняя строка `OK: ambar installed on 4 nodes`).

### Сквозная проверка

| Прогон | Время | Итог |
|---|---|---|
| 1 из 2 | 3 мин 11 с | `OK: csi e2e` |
| 2 из 2 | 2 мин 34 с | `OK: csi e2e` |

Шаг c2 (второй под с тем же PVC на узле вне тома, пока `dv-2` держит том смонтированным), прогон 1:

```
Warning   FailedMount   MountVolume.MountDevice failed for volume "pvc-3eb0a280-b2b2-4506-b818-3dc762093eb5" : rpc error: code = Unavailable desc = the volume is open on another node: cannot read /dev/drbd1000 (in use on another node): Wrong medium type (os error 124)
-- members with dup3 pending: lima-dv-1=Replica lima-dv-2=Replica lima-dv-3=Replica lima-dv-4=Client
time: Client entry of dv-4 removed 1 s after dup3 was deleted
-- members after dup3 was deleted: lima-dv-1=Replica lima-dv-2=Replica lima-dv-3=Replica
```

Код `Unavailable` вместо `FailedPrecondition` в прогонах 1 и 2. Шаг c2 только печатает наблюдение и код не проверяет, `stand/csi-e2e.sh` не менялся.

### Ручной сценарий: второй под на узле вне тома

Том `pvc-c6e411e2-db8a-4f17-84b8-6e8cf1bfb563` (`ambar-r2`, PVC `m2`). Под `p1` на `lima-dv-1` смонтировал том; состав: `lima-dv-1=Replica lima-dv-2=Replica lima-dv-3=Witness`; на `lima-dv-4` ресурса нет (`No such resource`). Затем под `p2` с тем же PVC на `lima-dv-4`.

Выборки `spec.nodes` каждые ~5 с (14 штук, с 19:48:46 по 19:49:57 UTC), все одинаковы:

```
19:48:46 spec.nodes: lima-dv-1=Replica lima-dv-2=Replica lima-dv-3=Witness lima-dv-4=Client
19:48:52 spec.nodes: lima-dv-1=Replica lima-dv-2=Replica lima-dv-3=Witness lima-dv-4=Client
...
19:49:57 spec.nodes: lima-dv-1=Replica lima-dv-2=Replica lima-dv-3=Witness lima-dv-4=Client
```

События `p2` через 77 с после создания:

```
LAST                   COUNT    TYPE      REASON        MSG
<nil>                  <none>   Normal    Scheduled     Successfully assigned default/p2 to lima-dv-4
2026-11-13T19:49:52Z   8        Warning   FailedMount   MountVolume.MountDevice failed for volume "pvc-c6e411e2-db8a-4f17-84b8-6e8cf1bfb563" : rpc error: code = Unavailable desc = the volume is open on another node: cannot read /dev/drbd1000 (in use on another node): Wrong medium type (os error 124)
NAME   READY   STATUS              RESTARTS   AGE
p2     0/1     ContainerCreating   0          77s
```

kubelet повторял `NodeStageVolume` (8 событий за 77 с); запись `lima-dv-4=Client` оставалась между повторами. `drbdsetup status` в это время:

```
-- dv-1
pvc-c6e411e2-db8a-4f17-84b8-6e8cf1bfb563 role:Primary
  disk:UpToDate open:yes
  lima-dv-2 role:Secondary
    peer-disk:UpToDate
  lima-dv-3 role:Secondary
    peer-disk:Diskless peer-client:yes
  lima-dv-4 role:Secondary
    peer-disk:Diskless peer-client:yes
-- dv-4
pvc-c6e411e2-db8a-4f17-84b8-6e8cf1bfb563 role:Secondary
  disk:Diskless client:yes open:no
  lima-dv-1 role:Primary
    peer-disk:UpToDate
  lima-dv-2 role:Secondary
    peer-disk:UpToDate
  lima-dv-3 role:Secondary
    peer-disk:Diskless peer-client:yes
```

После `kubectl delete pod p2 --wait=true` (19:50:10, под удалён к 19:50:12) запись `lima-dv-4=Client` отсутствовала уже в первой выборке (19:50:12), далее 20 выборок до 19:51:55 без неё; на `dv-4`: `No such resource`; на `dv-1` пира `lima-dv-4` нет. Кто именно снял запись (`NodeUnstageVolume` от kubelet), в выводах агента не видно: агент этот вызов не логирует; запись исчезла за время, которое занял `kubectl delete`, ≤ 2 с `[проверить: вызывает ли kubelet NodeUnstageVolume при каждой такой ошибке, или запись снял другой путь]`.

Условие снятия членства «`drbdadm down` отказывает при открытом устройстве». На `dv-1` при смонтированном томе (под `p1`):

```
19:52:11 drbdadm down на dv-1 (устройство открыто: p1 смонтировал том)
rc=11
pvc-c6e411e2-db8a-4f17-84b8-6e8cf1bfb563: State change failed: (-12) Device is held open by someone
additional info from kernel:
failed to demote
/dev/drbd1000 open_cnt:1, writable:1; list of openers follows
drbd1000 opened by mount (pid 454007) at 2026-11-13 19:48:37.078
Command 'drbdsetup down pvc-c6e411e2-db8a-4f17-84b8-6e8cf1bfb563' terminated with exit code 11
```

После отказа ресурс остался `Primary`, `open:yes`, файл `/data/f` в поде `p1` читается (`hi`). Один прогон.

### Удаление тома под, работавшего до удаления

Том `pvc-c6e411e2-...`, под `p1` удалён (19:52:19, `kubectl delete pod --wait=true`, 2 с), затем `kubectl delete pvc m2` (19:52:21): `ReplicatedVolume` исчез через 2 с (19:52:23). Строки журналов агентов (метки времени контейнера, UTC+8, то есть 03:52 = 19:52 UTC):

```
lima-dv-1: 03:52:22.222 ... the local copy is kept: peer lima-dv-3 is not connected and has not reported cleanup
lima-dv-1: 03:52:22.252 ... the local copy is kept: peer lima-dv-2 is not connected and has not reported cleanup; peer lima-dv-3 is not connected and has not reported cleanup
lima-dv-1: 03:52:22.280 ... the local copy is kept: peer lima-dv-2 is not connected and has not reported cleanup; peer lima-dv-3 is not connected and has not reported cleanup
lima-dv-1: 03:52:22.415 ... the local copy is kept: peer lima-dv-2 is not connected and has not reported cleanup
lima-dv-1: 03:52:22.725 ... removed the local copy ["down", "detach-loop", "remove-file", "remove-marker", "remove-synced-marker", "remove-link", "remove-res", "remove-applied"]
lima-dv-2: 03:52:22.465 ... removed the local copy [...]
lima-dv-3: 03:52:22.376 ... cleaned leftovers ["down", "remove-res", "remove-applied"]
```

Строк «kept» на `lima-dv-2`, `lima-dv-3`, `lima-dv-4` нет. Агент пишет «kept» только когда запись в `status.nodes` меняется, поэтому повторные проверки через 5 с в журнале не видны.

Второй случай, где причина — состояние DRBD, меняющееся без события на объекте: том `pvc-7bb1e65b-8a84-4728-a6e4-25e40ffeefbc` (PVC `m3`, под `p3` на `lima-dv-1`), `kubectl delete rv` при работающем поде (19:53:08), объект с двумя финализаторами `replica-lima-dv-1`, `replica-lima-dv-2` через 21 с всё ещё на месте (`this node is Primary` на `lima-dv-1`, `peer lima-dv-1 is Primary` на `lima-dv-2`). Под удалён в 19:53:29 (готов 19:53:30), объект исчез в 19:53:35, то есть через 5 с после удаления пода. Журналы: `lima-dv-1: 03:53:34.874 ... removed the local copy`, `lima-dv-2: 03:53:34.857 ... removed the local copy`. Один прогон; интервал 5 с совпадает с `KEPT_DRBD_DELAY`, но выборка из одного прогона не отделяет его от случайного совпадения.

### Состояние стенда после проверки

Четыре ВМ, драйвер установлен (четыре пода агента `3/3 Running`), объектов `ReplicatedVolume`, PVC, PV и подов в пространстве имён `default` нет, `NODES="dv-1 dv-2 dv-3 dv-4" stand/check.sh` печатает `OK: stand is up`. `stand/install.sh --remove` не запускался.

## Запись прогона на итоговом коммите фазы 2а

Стенд 2026-11-14: четыре ВМ (`dv-1`..`dv-4`), драйвер из `deploy/`, образ `ambar-drbd:dev`, собранный `image/build.sh` из чистого дерева с коммитом `245fc7b` (последний коммит фазы 2а до этой записи). Перед прогоном выполнены `image/build.sh` (20 с) и `NODES="dv-1 dv-2 dv-3 dv-4" stand/install.sh` (последняя строка `OK: ambar installed on 4 nodes`).

| Что | Значение |
|---|---|
| Коммит | `245fc7b` |
| Команда | `NODES="dv-1 dv-2 dv-3 dv-4" stand/csi-e2e.sh`, один прогон |
| Время | 291 с |
| Последняя строка | `OK: csi e2e` |
| Строк `forbidden` (без учёта регистра) в журналах всех трёх контейнеров (`ambar`, `node-driver-registrar`, `csi-provisioner`) четырёх подов агента | 0 из 12 журналов |
| Перезапуски контейнеров (`restartCount`) у четырёх подов, у каждого из трёх контейнеров | 0 |
| `NODES="dv-1 dv-2 dv-3 dv-4" stand/check.sh` | `OK: stand is up` |

Ограничение записи: сквозная проверка заменяет поды агентов на шаге с перезапуском под записывающим подом, поэтому к моменту `grep` возраст подов был 26–59 с и журналы покрывают только время после замены. Журналы подов до замены не сохранены; `restartCount` у новых подов не отражает замену самих подов. Строк `forbidden` в записи на момент до замены `[проверить: журналы подов до замены не читались]`.

Состояние стенда после прогона: четыре ВМ, драйвер установлен (четыре пода агента `3/3 Running`), объектов `ReplicatedVolume`, PV и PVC нет. `stand/install.sh --remove` не запускался, CRD и пространство имён `ambar-system` не удалялись.

## Проверки смены набора копий на стенде

Сценарии сквозной проверки реализованы в `stand/replica-e2e.sh` и охватывают:
1. `replica add`: добавление копии на узел-свидетель под непрерывной записью писателя (без сбоев и задержек записи);
2. `replica remove`: снятие копии и автоматическое назначение нового свидетеля, зачистку файлов и DRBD-ресурса на снятом узле;
3. отказ `replica remove` на узле с ролью `Primary` (под с томом);
4. `replica move`: перенос копии на свободный узел кластера с сохранением свидетеля (ровно 2 строки `edit:`); затем перенос на узел свидетеля с переназначением свидетеля;
5. прерывание `replica move` по таймауту `--timeout 20` во время синхронизации 1.5 ГиБ данных, отсутствие зависших процессов `ambar replica` и успешное доведение переноса повторной командой без таймаута (ровно 2 строки `edit:` суммарно);
6. соблюдение правил валидации CRD: запрет прямого удаления реплик, смены роли `Replica -> Client`, добавления `Retiring` извне и присутствия более одного `Retiring`;
7. проверку целостности данных после остановки писателя и чистое удаление тома;
8. отсутствие ошибок RBAC (`forbidden`) и непредвиденных перезапусков контейнеров.
