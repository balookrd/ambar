# Результаты проверки допущений DRBD 9 на стенде

Стенд: три ВМ Lima на маке, Debian 13 trixie arm64, ядро
`6.12.116+deb13-arm64`, DRBD 9.3.4, drbd-utils 9.35.0. Каждое допущение взято
из раздела «Что проверяется первым» спеки
`2026-11-12-duovol-design.md`.

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
| 4 | Сторона без диска участвует в кворуме и открывает устройство на запись | подтверждено: `dv-3` без диска в роли `Primary` пишет; при штатной остановке `dv-2` (шаг 8) и при принудительной (`limactl stop --force`, E2) кворум не потерян (`dv-3` + `dv-1`; на `dv-1` — `Would lose quorum, but using tiebreaker logic to keep`), запись стоит 4.6–5.6 с (5.1 с между выборками в обоих опытах) до `PingAck did not arrive in time` и продолжается; по одному прогону на каждый вид остановки | «Поведение при отказах», строка «Упал узел без пода»: пауза записи, вид остановки, сообщение о tiebreaker |
| 5 | `on-no-quorum`: `io-error` или `suspend-io` | выбран `io-error` (по одному прогону каждого режима). При `io-error` изолированный узел через 10–11 с получает `EIO` от `fsync`, `umount` проходит, узел уходит в `Secondary`. При `suspend-io` `sync` висит в `D` до возврата сети (77 с), `SIGKILL` не действует, `umount` → `target is busy`, устройство открыто, узел остаётся `Primary`, то есть освободить том на изолированном узле нельзя. `StandAlone` после возврата сети вызван ролью `Primary` в момент возврата, а не политикой: в опыте E1 при `io-error` с несмонтированным вовремя томом обе связи `dv-1` перешли в `StandAlone` с тем же `I shall become SyncTarget, but I am primary!`; восстановление — размонтирование и `drbdadm adjust` на всех трёх узлах. Подтверждённые записи на месте во всех опытах. `suspend-io` — значение по умолчанию в 9.3.4 | «Параметры ресурса DRBD»: `io-error` закреплён, строка в `.res` обязательна, описание ext4 по наблюдению; абзац о выборе разделён на политику и роль при возврате; «Поведение при отказах», строка «Узел с подом изолирован по `lan0`»: время, текст `dmesg`, `StandAlone` при возврате в роли `Primary`; абзац о split-brain: пометка `[проверить: …]`; «Монтирование»: код ошибки при открытии на втором узле |
| 6 | `drbdadm` и `drbdsetup` работают из контейнера | подтверждено предыдущей задачей: в поде `drbdadm --version` → `DRBD_KERNEL_VERSION=9.3.4`, `DRBDADM_VERSION=9.35.0`; в этой проверке все команды `drbdadm`/`drbdsetup` (`create-md`, `up`, `adjust`, `new-current-uuid`, `status --json`, `secondary`, `down`) выполнялись в поде `toolbox` | «Утилиты DRBD»: пометка о версии `drbd-utils` заменена на «проверено с 9.35.0» |

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
для двух пиров заняли 100 КиБ, из них 64 КиБ — битовая карта (вывод
`create-md`). Остальные 36 КиБ по выводу утилит не разложены. Сгенерированный файл `/var/lib/rancher/duovol/res/spike.res`:

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
vm dv-1 "grep on-no-quorum /var/lib/rancher/duovol/res/spike.res"
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
  volume:0 minor:1000 disk:UpToDate backing_dev:/dev/duovol/spike quorum:no
      open:yes blocked:no
  lima-dv-2 node-id:1 connection:StandAlone role:Unknown tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Off peer-disk:DUnknown resync-suspended:no
  lima-dv-3 node-id:2 connection:StandAlone role:Unknown tls:no congested:no
      ap-in-flight:0 rs-in-flight:0
    volume:0 replication:Off peer-disk:DUnknown resync-suspended:no

== dv-2
spike node-id:1 role:Primary suspended:no force-io-failures:no
  volume:0 minor:1000 disk:UpToDate backing_dev:/dev/duovol/spike quorum:yes
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

`drbdadm adjust spike` на `dv-2` (21:56:49.1) и `dv-3` (21:56:49.7), через 5 с
все три стороны `Connected`, диски `UpToDate`. `dmesg -T` на `dv-1`:

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
| Что восстановило | размонтирование на `dv-1`, затем `drbdadm adjust spike` на всех трёх узлах; `adjust` только на `dv-1` недостаточен |
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
| Остановка → `PingAck did not arrive in time` для `spike2` | ~4–5 с (остановка 21:57:46.1–46.3, сообщение в 21:57:50 на `dv-1` и 21:57:51 на `dv-3`, `dmesg -T` с точностью до секунды) |
| Связь с параметрами | `ping-int 10` с, `ping-timeout 0.5` с, `timeout 6` с; пауза короче `ping-int` и `timeout` [проверить: какой механизм DRBD отправляет ping раньше `ping-int` при ожидающей записи] |
| Кворум | сохранён на `dv-3` и `dv-1` |

Остановка писателя и сверка:

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
kc get node lima-dv-2; kc -n duovol-spike rollout status ds/toolbox --timeout=180s
tb dv-2 "/opt/duovol/load-module.sh"
vm dv-2 "mkdir -p /dev/duovol; for r in spike spike2; do f=/var/lib/rancher/duovol/volumes/\$r.img; ln -sfn \$(losetup --find --show --direct-io=on \$f) /dev/duovol/\$r; done; losetup -l -O NAME,BACK-FILE,DIO"
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
/dev/loop1 /var/lib/rancher/duovol/volumes/spike2.img   1
/dev/loop0 /var/lib/rancher/duovol/volumes/spike.img    1
21:59:20.971
13:59:21.707 spike role:Secondary disk:UpToDate quorum:no open:no  | spike2 role:Secondary disk:UpToDate quorum:no open:no lima-dv-1 connection:Connecting lima-dv-3 connection:Connecting
13:59:22.491 spike role:Secondary disk:UpToDate open:no  | spike2 role:Secondary disk:Inconsistent open:no lima-dv-1 role:Secondary replication:SyncTarget peer-disk:UpToDate done:55.93 lima-dv-3 role:Secondary peer-disk:Diskless peer-client:yes resync-suspended:dependency
13:59:22.755 spike role:Secondary disk:UpToDate open:no  | spike2 role:Secondary disk:UpToDate open:no lima-dv-1 role:Secondary peer-disk:UpToDate lima-dv-3 role:Secondary peer-disk:Diskless peer-client:yes
```

Метки `13:59` — время пода (UTC), соответствуют 21:59 на узлах. Узел вернулся
с первой попытки.

`dmesg -T | grep -i drbd` на `dv-2` целиком, от загрузки модуля до конца
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
for n in dv-1 dv-2 dv-3; do vm $n "cat /sys/module/drbd/version; ps -eo pid,comm,args | grep -E '[p]robe\.sh|[w]atch\.sh' || echo 'ps: no probe/watch'; losetup -l | wc -l; ls /dev/duovol; ls /usr/local/bin /var/tmp; nft list tables | grep spike || echo no-spike-table"; done
```

```text
== dv-1
# No currently configured DRBD found.
/var/lib/rancher/duovol/res:

/var/lib/rancher/duovol/volumes:
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
ls: cannot access '/dev/duovol': No such file or directory
no-spike-table
== dv-2
9.3.4
ps: no probe/watch
0
ls: cannot access '/dev/duovol': No such file or directory
no-spike-table
== dv-3
9.3.4
ps: no probe/watch
0
ls: cannot access '/dev/duovol': No such file or directory
no-spike-table
```

Вывод `ls /usr/local/bin /var/tmp` на каждом узле: в `/usr/local/bin` только
`crictl ctr k3s k3s-killall.sh k3s-uninstall.sh kubectl lima-guestagent`, в
`/var/tmp` только `cloud-init` и каталог `systemd-private-…`; `probe.sh`,
`watch.sh`, `fsync-bench.sh`, файлы `acked*`, `probe*.log`, `watch*.log` удалены.
