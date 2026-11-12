# Результаты проверки допущений DRBD 9 на стенде

Стенд: три ВМ Lima на маке, Debian 13 trixie arm64, ядро
`6.12.116+deb13-arm64`, DRBD 9.3.4, drbd-utils 9.35.0. Каждое допущение взято
из раздела «Что проверяется первым» спеки
`2026-11-12-duovol-design.md`.

## Окружение

| Что | Значение |
|---|---|
| Версия Lima | `limactl version 2.3.0`, драйвер `vz`, 2 CPU, 3 ГиБ RAM на ВМ |
| Версия k3s | `v1.37.2+k3s1`, три server |
| Интерфейс общей сети в ВМ | `eth0`, 192.168.104.1 / .3 / .4 (`dv-1` / `dv-2` / `dv-3`) |
| Имена хостов ВМ | `lima-dv-1`, `lima-dv-2`, `lima-dv-3`; `node-id` 0, 1, 2 |
| Порты ресурсов | `spike`: 7000, minor 1000; `spike2`: 7001, minor 1001 |
| Сетевые таймауты DRBD | по умолчанию: `ping-int 10` с, `ping-timeout 0.5` с, `timeout 6` с, `connect-int 10` с, `ko-count 7` |

## Итог по допущениям

| № | Допущение | Результат | Что меняется в спеке |
|---|---|---|---|
| 1 | Модуль 9.3.4 загружается с `usermode_helper=disabled` | подтверждено предыдущей задачей: модуль 9.3.4 загружен на трёх узлах, `/sys/module/drbd/parameters/usermode_helper` = `disabled`, в `dmesg` только предупреждения о модуле вне дерева ядра и без подписи; после перезапуска `dv-2` `load-module.sh` загрузил его снова (шаг 8) | ничего |
| 2 | Ресурс из трёх сторон поверх loop с `--direct-io=on` поднимается | подтверждено: `res.sh` без правок, `DIO` = `1` на трёх узлах, три стороны `Connected`; досылки после изоляции и выключения узла идут по битовой карте (32 КиБ; 16668 КиБ за ~30 с под нагрузкой) | «Поведение при отказах», строка «Узел вернулся»: диск проходит `Outdated` → `Inconsistent` → `UpToDate` |
| 3 | Начальная синхронизация пропускается через `new-current-uuid --clear-bitmap` | подтверждено: одна команда `drbdadm new-current-uuid --clear-bitmap <имя>/0` на `dv-1` после соединения всех сторон перевела все копии в `UpToDate` за < 4 с без досылки; так же для ресурса со стороной без диска | «Создание (`CreateVolume`)»: пометка `[проверить на стенде: …]` заменена наблюдаемой последовательностью |
| 4 | Сторона без диска участвует в кворуме и открывает устройство на запись | подтверждено: `dv-3` без диска в роли `Primary` пишет; при выключении `dv-2` кворум не потерян (`dv-3` + `dv-1`), запись стоит ~4.6 с до `PingAck` и продолжается | «Поведение при отказах», строка «Упал узел без пода»: пауза записи до обнаружения разрыва |
| 5 | `on-no-quorum`: `io-error` или `suspend-io` | `io-error`: изолированный узел через 10–11 с получает `EIO`, `umount` проходит, узел уходит в `Secondary`, после возврата сети всё соединяется само за ~6 с. При `suspend-io` `fsync` висит в `D` до возврата сети, `SIGKILL` не действует, `umount` → `target is busy`, устройство остаётся открытым и `Primary`; после возврата сети соединение с одним пиром остаётся `StandAlone` до ручного `drbdadm adjust` на обоих узлах. Подтверждённые записи на месте в обоих режимах. `suspend-io` — значение по умолчанию в 9.3.4 | «Параметры ресурса DRBD»: `io-error` закреплён, строка в `.res` обязательна, описание поведения ext4 исправлено; «Поведение при отказах», строка «Узел с подом изолирован по `lan0`»: время и текст `dmesg`; «Монтирование»: код ошибки при открытии на втором узле |
| 6 | `drbdadm` и `drbdsetup` работают из контейнера | подтверждено: в поде `drbdadm --version` → `DRBD_KERNEL_VERSION=9.3.4`, `DRBDADM_VERSION=9.35.0`; все команды `drbdadm`/`drbdsetup` этой проверки (`create-md`, `up`, `adjust`, `new-current-uuid`, `status --json`, `secondary`, `down`) выполнялись в поде `toolbox` | «Утилиты DRBD»: пометка о версии `drbd-utils` заменена на «проверено с 9.35.0» |

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

## Протокол

Команды с хоста выполнялись из корня репозитория через `bash -c 'source stand/lib.sh; …'`.
`vm` запускает команду от root в ВМ, `tb` — в поде `toolbox` этого узла.
Отметки времени `date +%T` и счётчики `[ N ]` в `dmesg` у каждой ВМ свои: время
`dmesg` отсчитывается от загрузки ВМ.

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
  volume:0 minor:1000 disk:Inconsistent backing_dev:/dev/duovol/spike quorum:yes
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
для двух пиров заняли 100 КиБ (битовая карта 64 КиБ, журнал активности и
суперблок). Сгенерированный файл `/var/lib/rancher/duovol/res/spike.res`:

```text
resource spike {
    options { quorum majority; on-no-quorum io-error; auto-promote yes; }
    net { protocol C; }
    on lima-dv-1 {
        node-id 0;
        address ipv4 192.168.104.1:7000;
        volume 0 { device minor 1000; disk /dev/duovol/spike; meta-disk internal; }
    }
    on lima-dv-2 {
        node-id 1;
        address ipv4 192.168.104.3:7000;
        volume 0 { device minor 1000; disk /dev/duovol/spike; meta-disk internal; }
    }
    on lima-dv-3 {
        node-id 2;
        address ipv4 192.168.104.4:7000;
        volume 0 { device minor 1000; disk /dev/duovol/spike; meta-disk internal; }
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
/dev/loop0 /var/lib/rancher/duovol/volumes/spike.img   1
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
той же ВМ.

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
| Изоляция → `PingAck did not arrive in time`, `quorum( yes -> no )` | ~10 с (21:34:40 → 21:34:50), соответствует `ping-int 10` + `ping-timeout 0.5` |
| Изоляция → писатель завершился с `EIO` | 10–11 с (`probe=gone` в 21:34:51.398) |
| Всё это время писатель | висит в `fsync` в состоянии `S`, номер не растёт |

`mount` на `dv-1` после ошибки показывает `rw`: журнал ext4 прерван
(`Aborting journal`), но перемонтирования только на чтение нет, потому что
`Errors behavior: Continue`.

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
  vm "$n" "sed -i 's/on-no-quorum io-error/on-no-quorum suspend-io/' /var/lib/rancher/duovol/res/spike.res"
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

| Наблюдение | `io-error` (шаг 6) | `suspend-io` (шаг 7) |
|---|---|---|
| Изоляция → потеря кворума | ~10 с | ~10 с |
| Писатель после потери кворума | `fsync` → `EIO` через 10–11 с после изоляции, процесс завершился | `sync` висит в `D` до возврата сети (77 с в опыте), `SIGKILL` не действует |
| `umount` на изолированном узле | `rc=0`, узел перешёл в `Secondary` | `target is busy`, `rc=32`; `umount -l` `rc=0`, но устройство открыто, узел `Primary` |
| Подтверждённые записи после монтирования на `dv-2` | ACKED 988, на томе 989 | ACKED 1046, на томе 1046 |
| Возврат сети → все соединения `Connected` | ~6 с, без вмешательства | `dv-1`↔`dv-2` за ~1 с; `dv-1`↔`dv-3` в `StandAlone` до ручного `drbdadm adjust` на обоих узлах |

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

Выключение `dv-2`:

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
`pgrep -x probe.sh` его не находит. `watch.sh` на `dv-3` вокруг выключения:

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
| Пауза записи при выключении `dv-2` | ~4.6 с (последний рост в 21:41:51.08, следующий в 21:41:56.19); запись ждёт подтверждения от `dv-2`, пока не сработает `PingAck` |
| Кворум у `dv-3` (без диска) + `dv-1` (с диском) | сохранён, `"quorum": true` |

Образцы: `stand/spike/samples/diskless-peer-down.json` (на `dv-3`, один пир
`Connecting`, `quorum: true`), `stand/spike/samples/peer-disconnected-diskful.json`
(на `dv-1`).

Возврат `dv-2`:

```bash
limactl start --tty=false dv-2
kc wait --for=condition=Ready node --all --timeout=180s
kc -n duovol-spike rollout status ds/toolbox --timeout=120s
tb dv-2 "/opt/duovol/load-module.sh"
vm dv-2 "mkdir -p /dev/duovol; for r in spike spike2; do f=/var/lib/rancher/duovol/volumes/\$r.img; ln -sfn \$(losetup --find --show --direct-io=on \$f) /dev/duovol/\$r; done"
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
/dev/loop1 /var/lib/rancher/duovol/volumes/spike2.img   1
/dev/loop0 /var/lib/rancher/duovol/volumes/spike.img    1
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

Писатель снят `SIGTERM` между записью и подтверждением: в журнале 60627
строк без пропусков, подтверждено 60626.

### Шаг 9. Замер `fsync` (ВМ, не R5C)

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
| `/var/tmp/local` (диск ВМ) | 919 мкс | 1323 мкс |
| `/mnt/spike` (DRBD, `protocol C`, три копии, loop с прямым вводом-выводом) | 3918 мкс | 2411 мкс |

Время включает запуск процесса `dd` на каждую запись. ВМ, не R5C; разброс
между прогонами до 1.6 раза.
