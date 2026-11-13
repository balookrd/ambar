# ambar, фаза 1: ядро агента без Kubernetes — план реализации

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Цель:** бинарь `ambar`, который на одном узле приводит один том к его описанию: создаёт файл и loop-устройство, пишет файл ресурса DRBD, поднимает ресурс, пропускает начальную синхронизацию, восстанавливает том после перезагрузки узла и удаляет его.

**Архитектура:** крейт `ambar` (библиотека и бинарь). Решение «что делать» — чистая функция `reconcile::plan` от описания тома и наблюдаемого состояния; внешние команды (`drbdadm`, `drbdsetup`, `losetup`, `insmod`) вызываются через трейт `exec::Runner`, который в тестах подменяется. Kubernetes в этой фазе нет: описание тома читается из JSON-файла, команды запускаются из пода `toolbox` на стенде.

**Стек:** Rust 1.99 (edition 2024), `serde`, `serde_json`, `clap`, `thiserror`, `anyhow`, `tempfile` (тесты), `cargo-zigbuild` для сборки под `aarch64-unknown-linux-musl`, DRBD 9.3.4, drbd-utils 9.35.0.

**Спека:** `docs/superpowers/specs/2026-11-12-ambar-design.md`. **Протокол опытов:** `docs/superpowers/specs/2026-11-12-drbd9-spike-results.md`. **Стенд:** `stand/README.md`.

## Место этой фазы

Агент из спеки разбит на четыре плана. Каждый даёт работающий и проверяемый результат.

| Фаза | Что даёт | Состояние |
|---|---|---|
| 1. Ядро агента (этот план) | модель тома, файл ресурса, разбор статуса, загрузчик модуля, локальное согласование, CLI; проверка на стенде без Kubernetes | план написан |
| 2. Kubernetes | CRD `ReplicatedVolume`, цикл согласования по объектам, CSI Controller и Node, манифесты, том через PVC на стенде | план не написан |
| 3. Отказы | освобождение тома при потере кворума, taint, `adjust` при `StandAlone`, метрики, сценарии отказов на стенде | план не написан |
| 4. Замер на R5C | память, `fsync`, сравнение с Longhorn | нужны узлы |

Что в этой фазе сознательно не делается: CRD и чтение объектов Kubernetes, CSI, монтирование файловой системы агентом, освобождение тома, taint, метрики, выбор узлов для копий, выделение `minor` и `port`.

Отличие от спеки, принятое для этой фазы: адрес узла на сети репликации записан в описании тома рядом с именем узла. В спеке адреса лежат в `ConfigMap ambar-nodes`; соединять их с томом будет контроллер фазы 2.

## Global Constraints

- Версия ядра: `6.12.116+deb13-arm64` (переменная `KVER` в скриптах). DRBD 9.3.4, drbd-utils 9.35.0. Версии не менять.
- В файле ресурса всегда явно: `quorum majority; on-no-quorum io-error; auto-promote yes;`. Значение `on-no-quorum` по умолчанию в 9.3.4 — `suspend-io`.
- `replicas` не меньше 2: описание тома с одним узлом с копией отклоняется.
- `drbdadm create-md` выполняется только для файла, у которого нет маркера готовности `<том>.img.ready`. Для файла с маркером он не вызывается никогда.
- `drbdadm new-current-uuid --clear-bitmap` выполняет только узел с наименьшим `nodeId` среди узлов с копией и только когда локальный диск `Inconsistent`, все стороны `Connected`, а у каждой стороны диск `Inconsistent` либо стороны без диска.
- Агент никогда не вызывает `drbdadm primary --force`.
- Результат команды `drbdadm`, меняющей состояние, определяется повторным чтением `drbdsetup status --json`, а не кодом возврата.
- Пути на узле: файлы томов `/var/lib/rancher/ambar/volumes/<том>.img`, файлы ресурсов `/etc/drbd.d/<том>.res` в контейнере (на узле `/var/lib/rancher/ambar/res`), ссылки на loop-устройства `/dev/ambar/<том>`.
- Loop-устройство подключается с `--direct-io=on`.
- Модуль загружается с параметром `usermode_helper=disabled`; модули лежат в `/opt/ambar/modules/<версия ядра>/`.
- Образ собирается с `--provenance=false --sbom=false`.
- Тесты лежат в `tests/` рядом с модулем и подключаются через `#[cfg(test)] #[path = "tests/<имя>.rs"] mod tests;`. Inline-блоков `mod tests { … }` нет.
- Гейт перед каждым коммитом: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`. Вывод тестов без предупреждений.
- Комментарии в коде и сообщения коммитов — на английском. Документы в `docs/superpowers/**` — на русском. `README.md` и `README.ru.md` правятся вместе.
- Репозиторий публичный (`github.com/balookrd/ambar`). В файлы и сообщения коммитов не попадают адреса домашней сети владельца, его домашний домен и путь домашнего каталога; адреса стенда `192.168.104.x` допустимы.
- `git commit` выполняется локально в `main`, без веток, без трейлера `Co-Authored-By` и подписей об авторстве. `git push` — только по отдельной команде владельца.
- Все команды выполняются на маке из корня репозитория `.`. Команды стенда запускаются через `bash -c '…'` или скриптом: `stand/lib.sh` написан для bash.

## Что проверено до написания плана

Код задач 2–7 собран и прогнан отдельным черновым крейтом на маке 2026-11-13: `cargo test` — 49 тестов проходят, `cargo clippy --all-targets -- -D warnings` и `cargo fmt --check` чисты, `cargo zigbuild --release --target aarch64-unknown-linux-musl` даёт статический бинарь 1.0 МиБ. Типы статуса разбирают все 19 образцов из `stand/spike/samples/`.

Не проверено: всё, что требует ядра Linux и стенда. Команды DRBD в коде взяты из протокола опытов, кроме четырёх мест, которые на стенде не запускались и закрываются задачей 1:

| Что заложено в код | Откуда | Задача 1 |
|---|---|---|
| `drbdadm create-md --force --max-peers=<n> <том>` | формат флага из документации drbd-utils | опыт P2 |
| размер метаданных `(ceil(секторы / 2^18) · 8 · max_peers + 72)` секторов | формула совпала с двумя замерами стенда (100 и 68 КиБ на 1 ГиБ) | опыт P2 |
| `drbdsetup status --json` без ресурсов печатает строку-комментарий и возвращает 0 | наблюдалось без `--json` | опыт P3 |
| `losetup` работает из привилегированного контейнера | на стенде `losetup` запускался на узле, не в контейнере | опыт P1 |

Скрипт `stand/agent-smoke.sh` из задачи 8 на стенде не запускался.

## Файлы

| Файл | Назначение |
|---|---|
| `Cargo.toml`, `rustfmt.toml` | крейт и форматирование (ширина 120) |
| `.github/workflows/ci.yml` | fmt, clippy, тесты, проверка скриптов |
| `src/lib.rs` | список модулей |
| `src/main.rs` | CLI: `load-module`, `apply`, `remove`, `status` |
| `src/model.rs` | описание тома и его проверка |
| `src/paths.rs` | пути файлов тома, ресурса и ссылки на устройство |
| `src/exec.rs` | трейт `Runner` и запуск процессов |
| `src/module_loader.rs` | загрузка модуля DRBD 9 |
| `src/backing.rs` | разреженный файл, loop-устройство, расчёт размера |
| `src/drbd/mod.rs` | модули DRBD |
| `src/drbd/resource.rs` | текст файла ресурса |
| `src/drbd/status.rs` | типы и разбор `drbdsetup status --json` |
| `src/drbd/adm.rs` | вызовы `drbdadm` и `drbdsetup` |
| `src/reconcile.rs` | `plan` и исполнитель |
| `src/tests/mod.rs` | `FakeRunner` для тестов |
| `src/tests/{model,module_loader,backing,reconcile}.rs`, `src/drbd/tests/{resource,status,adm}.rs` | тесты модулей |
| `image/Dockerfile`, `image/build.sh`, `image/test.sh`, `image/modprobe-blacklist.conf` | бинарь и чёрный список в образе |
| `stand/agent-smoke.sh` | сквозная проверка на стенде |
| `stand/spike/pre-agent.sh` | опыты P1–P6 |
| `README.md`, `README.ru.md` | состояние проекта |

---

### Task 1: опыты на стенде перед кодом (P1–P6)

Шесть коротких проверок закрывают то, на что опирается код следующих задач и чего нет в протоколе опытов. Это исследование: результат — запись того, что произошло.

**Files:**
- Create: `stand/spike/pre-agent.sh`
- Modify: `docs/superpowers/specs/2026-11-12-drbd9-spike-results.md` (новый раздел в конце), `docs/superpowers/specs/2026-11-12-ambar-design.md` (снять закрытые пометки `[проверить: …]`)

**Interfaces:**
- Consumes: стенд (`stand/README.md`), хелперы `vm`, `vm_put`, `vm_ip`, `vm_host`, `kc`, `tb`, массив `NODES` из `stand/lib.sh`; `stand/spike/res.sh <имя> <minor> <порт> <узел без диска|-> <io-error|suspend-io>`; `stand/spike/clean.sh`.
- Produces: раздел «Опыты перед агентом (P1–P6)» в протоколе с командами, выводом и таблицей итогов. Задачи 5–8 сверяют с ним четыре строки таблицы «Что проверено до написания плана».

- [ ] **Step 1: убедиться, что стенд поднят и чист**

```bash
stand/up.sh && stand/k3s.sh && stand/check.sh
```

Ожидается последняя строка `OK: stand is up`.

```bash
bash -c 'source stand/lib.sh; for n in "${NODES[@]}"; do tb "$n" "drbdsetup status; losetup -a"; done'
```

Ожидается на каждом узле `# No currently configured DRBD found.` и пустой список loop-устройств.

- [ ] **Step 2: написать `stand/spike/pre-agent.sh`**

```bash
#!/usr/bin/env bash
# Pre-agent checks on the stand. Each check cleans up before and after itself.
# usage: pre-agent.sh <p1|p2|p3|p4|p5|p6|clean>
#   p1  loop devices from inside the privileged container
#   p2  drbdadm create-md --max-peers and the resulting device size
#   p3  drbdsetup status --json with and without configured resources
#   p4  drbdadm adjust on a resource that is not up
#   p5  adding a node without a disk to a running resource
#   p6  drbdadm up on a backing file without metadata
set -euo pipefail
source "$(dirname "$0")/../lib.sh"

vol=/var/lib/rancher/ambar/volumes
res=/var/lib/rancher/ambar/res
names=(p1 p2 p3 p4 p5 p6)

say() { printf '\n### %s\n' "$*"; }

cleanup() {
    local n r
    for n in "${NODES[@]}"; do
        vm "$n" "pkill -f '^bash /usr/local/bin/(probe|watch)\\.sh' || true
            if mountpoint -q /mnt/pa; then umount /mnt/pa; fi"
        for r in "${names[@]}"; do
            tb "$n" "drbdsetup down $r 2>/dev/null || true"
        done
        vm "$n" "for r in ${names[*]}; do
                for d in \$(losetup -j $vol/\$r.img 2>/dev/null | cut -d: -f1); do losetup -d \$d; done
                rm -f /dev/ambar/\$r $vol/\$r.img $res/\$r.res
            done
            rm -f /var/tmp/pa-acked /var/tmp/pa-probe.log /var/tmp/pa-watch.log"
    done
}

# on_block <node> <node-id> <disk|none> <name> <minor> <port>
on_block() {
    local disk_line="disk none;"
    if [[ "$3" == disk ]]; then disk_line="disk /dev/ambar/$4; meta-disk internal;"; fi
    cat <<EOF
    on $(vm_host "$1") {
        node-id $2;
        address ipv4 $(vm_ip "$1"):$6;
        volume 0 { device minor $5; $disk_line }
    }
EOF
}

# write_res <name> <minor> <port> <target-nodes> <member>...
#   target-nodes: comma-separated nodes that receive the file
#   member: <node>:<node-id>:<disk|none>
write_res() {
    local name="$1" minor="$2" port="$3" targets="$4"; shift 4
    local body="" hosts="" m node id kind n
    for m in "$@"; do
        IFS=: read -r node id kind <<<"$m"
        body+="$(on_block "$node" "$id" "$kind" "$name" "$minor" "$port")"$'\n'
        hosts+=" $(vm_host "$node")"
    done
    local text
    text="resource $name {
    options { quorum majority; on-no-quorum io-error; auto-promote yes; }
    net { protocol C; }
${body}    connection-mesh { hosts$hosts; }
}"
    for n in ${targets//,/ }; do
        vm "$n" "mkdir -p $vol $res /dev/ambar"
        printf '%s\n' "$text" | vm_put "$n" "$res/$name.res"
    done
}

# backing <node> <name>: 1 GiB sparse file, loop device with direct I/O, link.
backing() {
    vm "$1" "f=$vol/$2.img
        truncate -s 1G \$f
        ln -sfn \$(losetup --find --show --direct-io=on \$f) /dev/ambar/$2"
}

# run_rc <node> <command>: run in the toolbox, print output and return code.
run_rc() {
    local node="$1"; shift
    echo "\$ [$node] $*"
    tb "$node" "$*; echo rc=\$?" || true
}

p1() {
    say "P1: loop devices from inside the container"
    run_rc dv-1 "truncate -s 64M $vol/p1.img && losetup --find --show --direct-io=on $vol/p1.img"
    run_rc dv-1 "losetup -l -O NAME,BACK-FILE,DIO | grep p1.img"
    run_rc dv-1 "losetup -j $vol/p1.img"
    echo "\$ [dv-1 host] losetup -j $vol/p1.img"
    vm dv-1 "losetup -j $vol/p1.img; echo rc=\$?"
    run_rc dv-1 "mkdir -p /dev/ambar && ln -sfn \$(losetup -j $vol/p1.img | cut -d: -f1) /dev/ambar/p1 && ls -l /dev/ambar/p1"
    echo "\$ [dv-1 host] ls -l /dev/ambar/p1"
    vm dv-1 "ls -l /dev/ambar/p1; echo rc=\$?"
    run_rc dv-1 "losetup -d \$(losetup -j $vol/p1.img | cut -d: -f1)"
}

p2() {
    say "P2: create-md --max-peers=7, device size for a 1 GiB file"
    write_res p2 1002 7002 dv-1,dv-2,dv-3 dv-1:0:disk dv-2:1:disk dv-3:2:disk
    local n
    for n in "${NODES[@]}"; do
        backing "$n" p2
        run_rc "$n" "drbdadm create-md --force --max-peers=7 p2"
        run_rc "$n" "drbdadm up p2"
    done
    sleep 5
    run_rc dv-1 "drbdsetup status p2 --json | grep -E '\"size\"|\"disk-state\"|\"connection-state\"'"
    echo "expected \"size\" by the formula (ceil(sectors/2^18)*8*7+72 sectors of metadata): 1048316"
    run_rc dv-1 "drbdadm new-current-uuid --clear-bitmap p2/0"
    sleep 3
    run_rc dv-1 "drbdsetup status p2 --json | grep -E '\"disk-state\"|\"peer-disk-state\"'"
}

p3() {
    say "P3: drbdsetup status --json without resources"
    run_rc dv-1 "drbdsetup status --json"
    run_rc dv-1 "drbdsetup status"
    run_rc dv-1 "drbdsetup status nosuch --json"
    say "P3: the same with one configured resource"
    write_res p3 1003 7003 dv-1 dv-1:0:disk dv-2:1:disk dv-3:2:disk
    backing dv-1 p3
    run_rc dv-1 "drbdadm create-md --force p3 && drbdadm up p3"
    run_rc dv-1 "drbdsetup status --json | head -c 300"
    run_rc dv-1 "drbdsetup status nosuch --json"
}

p4() {
    say "P4: drbdadm adjust on a resource that is not up"
    write_res p4 1004 7004 dv-1,dv-2,dv-3 dv-1:0:disk dv-2:1:disk dv-3:2:none
    local n
    for n in dv-1 dv-2; do
        backing "$n" p4
        run_rc "$n" "drbdadm create-md --force p4"
    done
    say "P4: node with a copy, resource never configured: adjust instead of up"
    run_rc dv-1 "drbdadm adjust p4"
    run_rc dv-1 "drbdsetup status p4"
    say "P4: second node with a copy: adjust"
    run_rc dv-2 "drbdadm adjust p4"
    say "P4: node without a disk: adjust"
    run_rc dv-3 "drbdadm adjust p4"
    sleep 5
    run_rc dv-1 "drbdsetup status p4 --verbose"
    run_rc dv-3 "drbdsetup status p4 --verbose"
}

p5() {
    say "P5: two-node resource with a writer, then a node without a disk is added"
    write_res p5 1005 7005 dv-1,dv-2 dv-1:0:disk dv-2:1:disk
    local n
    for n in dv-1 dv-2; do
        backing "$n" p5
        run_rc "$n" "drbdadm create-md --force --max-peers=7 p5"
        run_rc "$n" "drbdadm up p5"
    done
    sleep 5
    run_rc dv-1 "drbdadm new-current-uuid --clear-bitmap p5/0"
    sleep 3
    run_rc dv-1 "drbdsetup status p5 --verbose"
    tb dv-1 "mkfs.ext4 -q /dev/drbd1005"
    cat "$(dirname "$0")/probe.sh" | vm_put dv-1 /usr/local/bin/probe.sh
    cat "$(dirname "$0")/watch.sh" | vm_put dv-1 /usr/local/bin/watch.sh
    vm dv-1 "chmod +x /usr/local/bin/probe.sh /usr/local/bin/watch.sh
        mkdir -p /mnt/pa && mount /dev/drbd1005 /mnt/pa
        nohup bash /usr/local/bin/probe.sh /mnt/pa /var/tmp/pa-acked >/var/tmp/pa-probe.log 2>&1 &
        nohup bash /usr/local/bin/watch.sh /var/tmp/pa-acked >/var/tmp/pa-watch.log 2>&1 &
        sleep 5; date +%T.%3N; cat /var/tmp/pa-acked"

    say "P5: the resource file now lists three nodes; adjust on the two running nodes"
    write_res p5 1005 7005 dv-1,dv-2,dv-3 dv-1:0:disk dv-2:1:disk dv-3:2:none
    vm dv-1 "date +%T.%3N"
    run_rc dv-1 "drbdadm adjust p5"
    run_rc dv-2 "drbdadm adjust p5"
    say "P5: the new node brings the resource up without a disk"
    run_rc dv-3 "drbdadm up p5"
    sleep 8
    for n in "${NODES[@]}"; do run_rc "$n" "drbdsetup status p5 --verbose"; done
    say "P5: did the writer stall or fail while the node was added?"
    vm dv-1 "tail -n 40 /var/tmp/pa-watch.log; cat /var/tmp/pa-probe.log
        dmesg -T | grep -E 'drbd p5' | tail -n 30"
    say "P5: stop the writer, compare acknowledged and on-volume numbers, read through the new node"
    vm dv-1 "pkill -f '^bash /usr/local/bin/(probe|watch)\\.sh' || true; sleep 1
        echo acked=\$(cat /var/tmp/pa-acked) on-volume=\$(tail -n 1 /mnt/pa/log); umount /mnt/pa"
    vm dv-3 "mkdir -p /mnt/pa && mount /dev/drbd1005 /mnt/pa && echo on-volume-via-dv-3=\$(tail -n 1 /mnt/pa/log); umount /mnt/pa"
}

p6() {
    say "P6: drbdadm up on a backing file without metadata"
    write_res p6 1006 7006 dv-1 dv-1:0:disk dv-2:1:disk dv-3:2:disk
    backing dv-1 p6
    run_rc dv-1 "drbdadm up p6"
    run_rc dv-1 "drbdsetup status p6"
    run_rc dv-1 "drbdadm adjust p6"
}

case "${1:-}" in
    p1|p2|p3|p4|p5|p6) cleanup; "$1"; cleanup ;;
    clean) cleanup ;;
    *) echo "usage: $0 <p1|p2|p3|p4|p5|p6|clean>" >&2; exit 2 ;;
esac
```

- [ ] **Step 3: запустить опыты по одному и записать вывод**

```bash
chmod +x stand/spike/pre-agent.sh
for p in p1 p2 p3 p4 p5 p6; do stand/spike/pre-agent.sh $p 2>&1 | tee /tmp/pre-agent-$p.log; done
```

Скрипт на стенде не запускался. Если шаг опыта завершается ошибкой из-за самого скрипта (опечатка, неверный путь), исправьте скрипт и повторите опыт. Если ошибка — это поведение DRBD или `losetup`, запишите её как результат и не подбирайте команды до «ожидаемого» вывода.

Что ожидает код следующих задач и что делать при расхождении:

| Опыт | Ожидание | Если иначе |
|---|---|---|
| P1 | `losetup --find --show --direct-io=on` в контейнере печатает `/dev/loopN`, столбец `DIO` равен 1, `losetup -j <файл>` в контейнере и на узле называет то же устройство | остановиться: агент не сможет работать из контейнера; сообщить владельцу |
| P2 | `create-md --force --max-peers=7` завершается с кодом 0; `size` устройства для файла 1 ГиБ равен 1048316 КиБ | записать фактический синтаксис и размер; задача 6 правит `create_md` и `metadata_bytes` по записи |
| P3 | без ресурсов `drbdsetup status --json` возвращает 0 и не печатает JSON-массив; для несуществующего имени — ненулевой код | записать вывод и коды; задача 6 правит `Drbd::status` по записи |
| P4 | неизвестно: `drbdadm adjust` на неподнятом ресурсе либо поднимает его, либо отказывает | записать; код использует `up` для неподнятого ресурса и от исхода не зависит |
| P5 | неизвестно: добавление стороны без диска к работающему ресурсу | записать, что потребовалось и прерывалась ли запись; результат нужен фазе 2 |
| P6 | `drbdadm up` для файла без метаданных завершается с ненулевым кодом | записать текст ошибки и код |

- [ ] **Step 4: записать результаты в протокол**

В конец `docs/superpowers/specs/2026-11-12-drbd9-spike-results.md` добавить раздел «Опыты перед агентом (P1–P6)»: дата, версии, для каждого опыта — команда, вывод из `/tmp/pre-agent-<p>.log` в блоке кода с языком `text`, одна строка итога. В начале раздела — таблица: опыт | вопрос | результат. Стиль как в остальном протоколе: факты, «один прогон», неизвестный механизм помечается `[проверить: …]`.

- [ ] **Step 5: снять закрытые пометки в спеке**

В `docs/superpowers/specs/2026-11-12-ambar-design.md` заменить на результат опыта со ссылкой на раздел протокола:

- «Создание (`CreateVolume`)», пункт 1: `[проверить: формула размера внутренних метаданных; занимает ли место сторона без диска, заложенная в --max-peers]` — по P2;
- там же, пункт 3: `[проверить: поведение drbdadm adjust на ресурсе, который не поднят]` и пометку про сторону без диска — по P4;
- «Старт агента после перезагрузки узла»: пометку про `adjust` — по P4;
- «Добавление узла в кластер», пункт 3 — по P5 в той части, которую опыт показал; остальное остаётся с пометкой;
- строку `create-md --max-peers` в «Открытых вопросах»: дополнить столбец «что наблюдалось» результатом P2. Значение запаса остаётся открытым вопросом.

Изменять можно только эти места. Решений дизайна не принимать.

- [ ] **Step 6: очистить стенд и проверить**

```bash
stand/spike/pre-agent.sh clean && stand/spike/clean.sh && stand/check.sh
```

Ожидается `OK: stand is up`, на всех узлах нет ресурсов DRBD и loop-устройств.

- [ ] **Step 7: коммит**

```bash
git add stand/spike/pre-agent.sh docs/superpowers/specs/
git commit -m "docs(spike): pre-agent checks for loop devices, max-peers, status output and adding a node" -- stand/spike/pre-agent.sh docs/superpowers/specs/
```

---

### Task 2: крейт, модель тома и CI

**Files:**
- Create: `Cargo.toml`, `rustfmt.toml`, `src/lib.rs`, `src/main.rs`, `src/model.rs`, `src/tests/model.rs`, `.github/workflows/ci.yml`

**Interfaces:**
- Consumes: ничего.
- Produces (`ambar::model`):
  - `enum Protocol { A, C }`;
  - `struct VolumeNode { name: String, address: IpAddr, node_id: u32, diskful: bool }`;
  - `struct VolumeSpec { name: String, size_bytes: u64, protocol: Protocol, minor: u32, port: u16, max_peers: u32, nodes: Vec<VolumeNode> }`, поля JSON в `camelCase`, неизвестные поля отклоняются;
  - `VolumeSpec::from_json(&str) -> Result<VolumeSpec, SpecError>` (разбор и проверка), `validate(&self) -> Result<(), SpecError>`, `node(&self, name: &str) -> Option<&VolumeNode>`, `diskful_nodes(&self) -> impl Iterator<Item = &VolumeNode>`, `initial_sync_leader(&self) -> Option<&VolumeNode>`;
  - `enum SpecError` с вариантами `Json(String)`, `Name(String)`, `Size(u64)`, `Minor(u32)`, `Port(u16)`, `DuplicateName(String)`, `DuplicateNodeId(u32)`, `NodeId(u32)`, `Replicas(usize)`, `MaxPeers { max_peers: u32, needed: u32 }`;
  - константы `MAX_NODE_ID = 31`, `MAX_MINOR = 1048575`, `MIN_DISKFUL = 2`.

- [ ] **Step 1: создать крейт**

```bash
cargo init --name ambar --vcs none
cargo add serde --features derive
cargo add serde_json thiserror
cargo add clap --features derive
printf 'max_width = 120\n' > rustfmt.toml
```

Проверить, что в `Cargo.toml` стоит `edition = "2024"`, а `.gitignore` содержит `/target/` (строка уже есть).

- [ ] **Step 2: написать тесты модели**

`src/tests/model.rs`:

```rust
use super::*;

const SAMPLE: &str = r#"{
  "name": "spike",
  "sizeBytes": 1073741824,
  "protocol": "C",
  "minor": 1000,
  "port": 7000,
  "maxPeers": 7,
  "nodes": [
    {"name": "lima-dv-1", "address": "192.168.104.1", "nodeId": 0, "diskful": true},
    {"name": "lima-dv-2", "address": "192.168.104.3", "nodeId": 1, "diskful": true},
    {"name": "lima-dv-3", "address": "192.168.104.4", "nodeId": 2, "diskful": false}
  ]
}"#;

fn sample() -> VolumeSpec {
    VolumeSpec::from_json(SAMPLE).expect("sample is valid")
}

#[test]
fn parses_a_valid_volume() {
    let spec = sample();
    assert_eq!(spec.name, "spike");
    assert_eq!(spec.protocol, Protocol::C);
    assert_eq!(spec.nodes.len(), 3);
    assert_eq!(spec.diskful_nodes().count(), 2);
    assert_eq!(spec.node("lima-dv-3").map(|n| n.diskful), Some(false));
    assert_eq!(spec.node("nosuch"), None);
}

#[test]
fn the_lowest_diskful_node_id_leads_the_initial_sync() {
    let mut spec = sample();
    assert_eq!(spec.initial_sync_leader().map(|n| n.name.as_str()), Some("lima-dv-1"));
    spec.nodes[0].diskful = false;
    spec.nodes[2].diskful = true;
    assert_eq!(spec.initial_sync_leader().map(|n| n.name.as_str()), Some("lima-dv-2"));
}

#[test]
fn rejects_a_single_copy() {
    let mut spec = sample();
    spec.nodes[1].diskful = false;
    assert_eq!(spec.validate(), Err(SpecError::Replicas(1)));
}

#[test]
fn rejects_duplicate_node_ids_and_names() {
    let mut spec = sample();
    spec.nodes[2].node_id = 0;
    assert_eq!(spec.validate(), Err(SpecError::DuplicateNodeId(0)));

    let mut spec = sample();
    spec.nodes[2].name = "lima-dv-1".into();
    assert_eq!(spec.validate(), Err(SpecError::DuplicateName("lima-dv-1".into())));
}

#[test]
fn rejects_out_of_range_values() {
    let mut spec = sample();
    spec.name = "Spike".into();
    assert_eq!(spec.validate(), Err(SpecError::Name("Spike".into())));

    let mut spec = sample();
    spec.size_bytes = 1000;
    assert_eq!(spec.validate(), Err(SpecError::Size(1000)));

    let mut spec = sample();
    spec.minor = MAX_MINOR + 1;
    assert_eq!(spec.validate(), Err(SpecError::Minor(MAX_MINOR + 1)));

    let mut spec = sample();
    spec.port = 80;
    assert_eq!(spec.validate(), Err(SpecError::Port(80)));

    let mut spec = sample();
    spec.nodes[0].node_id = 32;
    assert_eq!(spec.validate(), Err(SpecError::NodeId(32)));
}

#[test]
fn max_peers_must_cover_the_other_nodes() {
    let mut spec = sample();
    spec.max_peers = 1;
    assert_eq!(
        spec.validate(),
        Err(SpecError::MaxPeers {
            max_peers: 1,
            needed: 2
        })
    );
    spec.max_peers = 32;
    assert_eq!(
        spec.validate(),
        Err(SpecError::MaxPeers {
            max_peers: 32,
            needed: 2
        })
    );
}

#[test]
fn rejects_unknown_fields() {
    let json = SAMPLE.replace("\"minor\": 1000,", "\"minor\": 1000, \"mirror\": 1,");
    assert!(matches!(VolumeSpec::from_json(&json), Err(SpecError::Json(_))));
}
```

- [ ] **Step 3: подключить модуль и убедиться, что тесты не собираются**

`src/lib.rs`:

```rust
//! ambar: replicated volumes for k3s on top of DRBD 9.

pub mod model;
```

`src/model.rs` — пока только подключение тестов:

```rust
#[cfg(test)]
#[path = "tests/model.rs"]
mod tests;
```

```bash
cargo test 2>&1 | grep -E "^error" | head -3
```

Ожидается ошибка компиляции: `cannot find type VolumeSpec` (тип ещё не определён).

- [ ] **Step 4: написать модель**

`src/model.rs` целиком:

```rust
//! Desired state of one replicated volume, as the agent reads it.

use std::collections::HashSet;
use std::net::IpAddr;

use serde::{Deserialize, Serialize};

/// DRBD 9 addresses at most 32 nodes per resource: node ids 0..=31.
pub const MAX_NODE_ID: u32 = 31;
/// Largest device minor number the kernel accepts (20 bits).
pub const MAX_MINOR: u32 = (1 << 20) - 1;
/// A volume keeps at least two copies; single-copy volumes stay on local-path.
pub const MIN_DISKFUL: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Protocol {
    A,
    C,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VolumeNode {
    /// Node name as `uname -n` reports it; DRBD matches `on <name>` against it.
    pub name: String,
    /// Address on the replication network.
    pub address: IpAddr,
    pub node_id: u32,
    /// `true`: the node holds a copy; `false`: it attaches without a disk.
    pub diskful: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VolumeSpec {
    pub name: String,
    pub size_bytes: u64,
    pub protocol: Protocol,
    pub minor: u32,
    pub port: u16,
    /// Number of peers the DRBD metadata is sized for (`create-md --max-peers`).
    pub max_peers: u32,
    pub nodes: Vec<VolumeNode>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SpecError {
    #[error("invalid volume json: {0}")]
    Json(String),
    #[error("invalid volume name {0:?}: use 1-63 characters a-z, 0-9 and '-', starting with a letter or digit")]
    Name(String),
    #[error("sizeBytes {0} must be a positive multiple of 4096")]
    Size(u64),
    #[error("minor {0} is above 1048575")]
    Minor(u32),
    #[error("port {0} is below 1024")]
    Port(u16),
    #[error("duplicate node name {0:?}")]
    DuplicateName(String),
    #[error("duplicate nodeId {0}")]
    DuplicateNodeId(u32),
    #[error("nodeId {0} is above 31")]
    NodeId(u32),
    #[error("{0} nodes hold a copy, at least 2 are required")]
    Replicas(usize),
    #[error("maxPeers {max_peers} must be between {needed} (other nodes of this volume) and 31")]
    MaxPeers { max_peers: u32, needed: u32 },
}

impl VolumeSpec {
    /// Parses and validates a volume description.
    pub fn from_json(json: &str) -> Result<Self, SpecError> {
        let spec: Self = serde_json::from_str(json).map_err(|e| SpecError::Json(e.to_string()))?;
        spec.validate()?;
        Ok(spec)
    }

    pub fn validate(&self) -> Result<(), SpecError> {
        if !valid_name(&self.name) {
            return Err(SpecError::Name(self.name.clone()));
        }
        if self.size_bytes == 0 || !self.size_bytes.is_multiple_of(4096) {
            return Err(SpecError::Size(self.size_bytes));
        }
        if self.minor > MAX_MINOR {
            return Err(SpecError::Minor(self.minor));
        }
        if self.port < 1024 {
            return Err(SpecError::Port(self.port));
        }
        let mut names = HashSet::new();
        let mut ids = HashSet::new();
        for node in &self.nodes {
            if !names.insert(node.name.as_str()) {
                return Err(SpecError::DuplicateName(node.name.clone()));
            }
            if node.node_id > MAX_NODE_ID {
                return Err(SpecError::NodeId(node.node_id));
            }
            if !ids.insert(node.node_id) {
                return Err(SpecError::DuplicateNodeId(node.node_id));
            }
        }
        let diskful = self.diskful_nodes().count();
        if diskful < MIN_DISKFUL {
            return Err(SpecError::Replicas(diskful));
        }
        let needed = self.nodes.len() as u32 - 1;
        if self.max_peers < needed || self.max_peers > MAX_NODE_ID {
            return Err(SpecError::MaxPeers {
                max_peers: self.max_peers,
                needed,
            });
        }
        Ok(())
    }

    pub fn node(&self, name: &str) -> Option<&VolumeNode> {
        self.nodes.iter().find(|n| n.name == name)
    }

    pub fn diskful_nodes(&self) -> impl Iterator<Item = &VolumeNode> {
        self.nodes.iter().filter(|n| n.diskful)
    }

    /// The diskful node with the lowest node id runs the one-time
    /// "skip initial sync" command for a freshly created volume.
    pub fn initial_sync_leader(&self) -> Option<&VolumeNode> {
        self.diskful_nodes().min_by_key(|n| n.node_id)
    }
}

fn valid_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    let allowed = |b: &u8| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-';
    !bytes.is_empty() && bytes.len() <= 63 && bytes[0] != b'-' && bytes.iter().all(allowed)
}

#[cfg(test)]
#[path = "tests/model.rs"]
mod tests;
```

- [ ] **Step 5: минимальный бинарь**

`src/main.rs`:

```rust
use clap::Parser;

#[derive(Parser)]
#[command(name = "ambar", version, about = "Replicated volumes for k3s on top of DRBD 9")]
struct Cli {}

fn main() {
    Cli::parse();
}
```

- [ ] **Step 6: прогнать гейт**

```bash
cargo fmt && cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test 2>&1 | grep -E "^test result" | head -1
```

Ожидается: `test result: ok. 7 passed; 0 failed`.

```bash
cargo run -q -- --version
```

Ожидается: `ambar 0.1.0`.

- [ ] **Step 7: добавить CI**

`.github/workflows/ci.yml`:

```yaml
name: ci

on:
  push:
    branches: [main]
  pull_request:

permissions:
  contents: read

jobs:
  rust:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
      - uses: dtolnay/rust-toolchain@4360b52568e2003a75bf9bc1d59f33a8e3fc893c # stable
        with:
          components: rustfmt, clippy
      - uses: Swatinem/rust-cache@6323deb102c322ba6fcbdcafc7e3dddab59af2b6 # v2.9.2
      - run: cargo fmt --check
      - run: cargo clippy --all-targets -- -D warnings
      - run: cargo test

  image-scripts:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
      - run: image/test-loader.sh
      - run: bash -n image/*.sh stand/*.sh stand/spike/*.sh
```

Версии actions закреплены по хешам коммитов. Вторая задача CI запускает `image/test-loader.sh` (ему не нужны Docker и ядро Linux) и проверяет синтаксис скриптов.

- [ ] **Step 8: коммит**

```bash
git add Cargo.toml Cargo.lock rustfmt.toml src .github
git commit -m "feat(agent): crate skeleton and the volume model" -- Cargo.toml Cargo.lock rustfmt.toml src .github
```

---

### Task 3: файл ресурса DRBD

**Files:**
- Create: `src/drbd/mod.rs`, `src/drbd/resource.rs`, `src/drbd/tests/resource.rs`
- Modify: `src/lib.rs`

**Interfaces:**
- Consumes: `ambar::model::{VolumeSpec, VolumeNode, Protocol}`.
- Produces: `ambar::drbd::resource::render(spec: &VolumeSpec, backing_device: &Path) -> String` — текст `<том>.res`, одинаковый для всех узлов тома; узлы упорядочены по `node_id`.

- [ ] **Step 1: написать тесты**

`src/drbd/tests/resource.rs`:

```rust
use std::path::Path;

use super::*;
use crate::model::VolumeSpec;

const TWO_COPIES_ONE_DISKLESS: &str = r#"{
  "name": "spike2",
  "sizeBytes": 1073741824,
  "protocol": "C",
  "minor": 1001,
  "port": 7001,
  "maxPeers": 7,
  "nodes": [
    {"name": "lima-dv-3", "address": "192.168.104.4", "nodeId": 2, "diskful": false},
    {"name": "lima-dv-1", "address": "192.168.104.1", "nodeId": 0, "diskful": true},
    {"name": "lima-dv-2", "address": "192.168.104.3", "nodeId": 1, "diskful": true}
  ]
}"#;

#[test]
fn renders_two_copies_and_a_diskless_node_sorted_by_node_id() {
    let spec = VolumeSpec::from_json(TWO_COPIES_ONE_DISKLESS).unwrap();
    let expected = "\
resource spike2 {
    options { quorum majority; on-no-quorum io-error; auto-promote yes; }
    net { protocol C; }
    on lima-dv-1 {
        node-id 0;
        address ipv4 192.168.104.1:7001;
        volume 0 { device minor 1001; disk /dev/ambar/spike2; meta-disk internal; }
    }
    on lima-dv-2 {
        node-id 1;
        address ipv4 192.168.104.3:7001;
        volume 0 { device minor 1001; disk /dev/ambar/spike2; meta-disk internal; }
    }
    on lima-dv-3 {
        node-id 2;
        address ipv4 192.168.104.4:7001;
        volume 0 { device minor 1001; disk none; }
    }
    connection-mesh { hosts lima-dv-1 lima-dv-2 lima-dv-3; }
}
";
    assert_eq!(render(&spec, Path::new("/dev/ambar/spike2")), expected);
}

#[test]
fn renders_protocol_a_and_ipv6_addresses() {
    let json = TWO_COPIES_ONE_DISKLESS
        .replace("\"protocol\": \"C\"", "\"protocol\": \"A\"")
        .replace("192.168.104.1", "fd00::1");
    let spec = VolumeSpec::from_json(&json).unwrap();
    let text = render(&spec, Path::new("/dev/ambar/spike2"));
    assert!(text.contains("    net { protocol A; }\n"), "{text}");
    assert!(text.contains("        address ipv6 [fd00::1]:7001;\n"), "{text}");
}

#[test]
fn the_quorum_policy_is_always_explicit() {
    let spec = VolumeSpec::from_json(TWO_COPIES_ONE_DISKLESS).unwrap();
    let text = render(&spec, Path::new("/dev/ambar/spike2"));
    assert!(text.contains("quorum majority; on-no-quorum io-error;"), "{text}");
}
```

Ожидаемый текст в первом тесте совпадает по строению с файлом, который создавал `stand/spike/res.sh` и с которым проведены все опыты.

- [ ] **Step 2: подключить модули и убедиться, что тесты не собираются**

В `src/lib.rs` добавить строку `pub mod drbd;` перед `pub mod model;`.

`src/drbd/mod.rs`:

```rust
//! Everything that talks to or about DRBD.

pub mod resource;
```

`src/drbd/resource.rs` — пока только подключение тестов:

```rust
#[cfg(test)]
#[path = "tests/resource.rs"]
mod tests;
```

```bash
cargo test 2>&1 | grep -E "^error" | head -3
```

Ожидается ошибка компиляции: `cannot find function render`.

- [ ] **Step 3: написать генерацию**

`src/drbd/resource.rs` целиком:

```rust
//! Renders the DRBD resource file of a volume.

use std::fmt::Write as _;
use std::net::IpAddr;
use std::path::Path;

use crate::model::{Protocol, VolumeNode, VolumeSpec};

/// Renders `<volume>.res`. The same text is written on every node of the
/// volume; DRBD picks the `on <host>` section that matches `uname -n`.
///
/// `backing_device` is the stable path of the loop device on a node that
/// holds a copy, e.g. `/dev/ambar/<volume>`.
pub fn render(spec: &VolumeSpec, backing_device: &Path) -> String {
    let mut nodes: Vec<&VolumeNode> = spec.nodes.iter().collect();
    nodes.sort_by_key(|n| n.node_id);

    let protocol = match spec.protocol {
        Protocol::A => "A",
        Protocol::C => "C",
    };

    let mut out = String::new();
    let _ = writeln!(out, "resource {} {{", spec.name);
    // `on-no-quorum io-error` is spelled out on purpose: DRBD 9.3.4 defaults
    // to `suspend-io`, which leaves the device open on an isolated node.
    let _ = writeln!(
        out,
        "    options {{ quorum majority; on-no-quorum io-error; auto-promote yes; }}"
    );
    let _ = writeln!(out, "    net {{ protocol {protocol}; }}");
    for node in &nodes {
        let disk = if node.diskful {
            format!("disk {}; meta-disk internal;", backing_device.display())
        } else {
            "disk none;".to_string()
        };
        let _ = writeln!(out, "    on {} {{", node.name);
        let _ = writeln!(out, "        node-id {};", node.node_id);
        let _ = writeln!(out, "        address {};", address(node.address, spec.port));
        let _ = writeln!(out, "        volume 0 {{ device minor {}; {disk} }}", spec.minor);
        let _ = writeln!(out, "    }}");
    }
    let hosts: Vec<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
    let _ = writeln!(out, "    connection-mesh {{ hosts {}; }}", hosts.join(" "));
    let _ = writeln!(out, "}}");
    out
}

fn address(ip: IpAddr, port: u16) -> String {
    match ip {
        IpAddr::V4(v4) => format!("ipv4 {v4}:{port}"),
        IpAddr::V6(v6) => format!("ipv6 [{v6}]:{port}"),
    }
}

#[cfg(test)]
#[path = "tests/resource.rs"]
mod tests;
```

- [ ] **Step 4: прогнать гейт**

```bash
cargo fmt && cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test 2>&1 | grep -E "^test result" | head -1
```

Ожидается: `test result: ok. 10 passed; 0 failed`.

- [ ] **Step 5: коммит**

```bash
git add src
git commit -m "feat(agent): render the DRBD resource file of a volume" -- src
```

---

### Task 4: разбор `drbdsetup status --json`

**Files:**
- Create: `src/drbd/status.rs`, `src/drbd/tests/status.rs`
- Modify: `src/drbd/mod.rs`

**Interfaces:**
- Consumes: образцы `stand/spike/samples/*.json` (19 файлов, описаны в `stand/spike/samples/README.md`).
- Produces (`ambar::drbd::status`):
  - `enum Role { Primary, Secondary, Unknown, Other }`;
  - `enum DiskState { Diskless, Attaching, Detaching, Failed, Negotiating, Inconsistent, Outdated, DUnknown, Consistent, UpToDate, Other }`;
  - `enum ConnectionState { StandAlone, Disconnecting, Unconnected, Timeout, BrokenPipe, NetworkFailure, ProtocolError, TearDown, Connecting, Connected, Other }`;
  - `struct Device { volume: u32, minor: u32, disk_state: DiskState, client: bool, open: bool, quorum: bool, size: u64 }`;
  - `struct PeerDevice { volume: u32, replication_state: String, peer_disk_state: DiskState, peer_client: bool, out_of_sync: u64 }`;
  - `struct Connection { peer_node_id: u32, name: String, connection_state: ConnectionState, peer_role: Role, peer_devices: Vec<PeerDevice> }`;
  - `struct ResourceStatus { name: String, node_id: u32, role: Role, suspended: bool, devices: Vec<Device>, connections: Vec<Connection> }`;
  - `parse(json: &str) -> Result<Vec<ResourceStatus>, serde_json::Error>`;
  - методы `ResourceStatus`: `device() -> Option<&Device>`, `quorum() -> bool`, `all_peers_connected() -> bool`, `standalone_peers() -> Vec<&str>`, `ready_to_skip_initial_sync() -> bool`.

Неизвестные значения перечислений разбираются в `Other`, неизвестные ключи JSON игнорируются: вывод более новой версии DRBD не ломает разбор.

- [ ] **Step 1: написать тесты на образцах**

`src/drbd/tests/status.rs`:

```rust
use super::*;

macro_rules! sample {
    ($name:literal) => {
        include_str!(concat!("../../../stand/spike/samples/", $name, ".json"))
    };
}

fn one(json: &str) -> ResourceStatus {
    let mut all = parse(json).expect("sample parses");
    assert_eq!(all.len(), 1);
    all.remove(0)
}

const ALL: [&str; 19] = [
    sample!("three-inconsistent"),
    sample!("three-uptodate"),
    sample!("primary-mounted"),
    sample!("peer-disconnected"),
    sample!("quorum-lost"),
    sample!("quorum-suspended"),
    sample!("peer-primary"),
    sample!("diskless-node"),
    sample!("diskless-peer-down"),
    sample!("peer-disconnected-diskful"),
    sample!("resync-target"),
    sample!("standalone-primary"),
    sample!("diskless-peer-killed"),
    sample!("diskless-alone-no-quorum"),
    sample!("stale-diskful-no-quorum"),
    sample!("diskless-stale-peer"),
    sample!("diskless-rebooted-stale-peer"),
    sample!("split-brain-victim"),
    sample!("split-brain-survivor"),
];

#[test]
fn every_captured_sample_parses_without_unknown_states() {
    for json in ALL {
        let st = one(json);
        assert_ne!(st.role, Role::Other, "{}", st.name);
        let dev = st.device().expect("volume 0");
        assert_ne!(dev.disk_state, DiskState::Other, "{}", st.name);
        assert_eq!(st.connections.len(), 2, "{}", st.name);
        for conn in &st.connections {
            assert_ne!(conn.connection_state, ConnectionState::Other, "{}", st.name);
            assert_ne!(conn.peer_role, Role::Other, "{}", st.name);
            assert_ne!(conn.peer_devices[0].peer_disk_state, DiskState::Other, "{}", st.name);
        }
    }
}

#[test]
fn a_mounted_primary_is_open_with_quorum() {
    let st = one(sample!("primary-mounted"));
    assert_eq!(st.name, "spike");
    assert_eq!(st.node_id, 0);
    assert_eq!(st.role, Role::Primary);
    let dev = st.device().unwrap();
    assert_eq!(
        (dev.minor, dev.disk_state, dev.open, dev.client),
        (1000, DiskState::UpToDate, true, false)
    );
    assert_eq!(dev.size, 1_048_476);
    assert!(st.quorum());
    assert!(st.all_peers_connected());
    assert!(st.standalone_peers().is_empty());
}

#[test]
fn an_isolated_primary_has_no_quorum_and_connecting_peers() {
    let st = one(sample!("quorum-lost"));
    assert_eq!(st.role, Role::Primary);
    assert!(!st.quorum());
    assert!(!st.all_peers_connected());
    assert!(
        st.connections
            .iter()
            .all(|c| c.connection_state == ConnectionState::Connecting)
    );
    assert!(st.standalone_peers().is_empty());
}

#[test]
fn standalone_connections_are_reported_by_peer_name() {
    assert_eq!(
        one(sample!("standalone-primary")).standalone_peers(),
        ["lima-dv-2", "lima-dv-3"]
    );
    assert_eq!(one(sample!("split-brain-survivor")).standalone_peers(), ["lima-dv-1"]);
}

#[test]
fn a_diskless_node_is_a_client_without_a_disk() {
    let st = one(sample!("diskless-node"));
    let dev = st.device().unwrap();
    assert!(dev.client);
    assert_eq!(dev.disk_state, DiskState::Diskless);
    assert!(st.quorum());
}

#[test]
fn a_resync_target_reports_what_is_left() {
    let st = one(sample!("resync-target"));
    assert_eq!(st.device().unwrap().disk_state, DiskState::Inconsistent);
    let source = st.connections.iter().find(|c| c.name == "lima-dv-1").unwrap();
    assert_eq!(source.peer_devices[0].replication_state, "SyncTarget");
    assert_eq!(source.peer_devices[0].out_of_sync, 4100);
    let diskless = st.connections.iter().find(|c| c.name == "lima-dv-3").unwrap();
    assert!(diskless.peer_devices[0].peer_client);
}

#[test]
fn only_a_fresh_fully_connected_volume_may_skip_the_initial_sync() {
    assert!(one(sample!("three-inconsistent")).ready_to_skip_initial_sync());

    // Already synchronised, resyncing, isolated, stale or split: never.
    for name in [
        sample!("three-uptodate"),
        sample!("primary-mounted"),
        sample!("resync-target"),
        sample!("quorum-lost"),
        sample!("stale-diskful-no-quorum"),
        sample!("standalone-primary"),
        sample!("split-brain-victim"),
        sample!("diskless-node"),
    ] {
        let st = one(name);
        assert!(!st.ready_to_skip_initial_sync(), "{} on node {}", st.name, st.node_id);
    }
}

#[test]
fn unknown_states_do_not_break_parsing() {
    let json = sample!("three-uptodate")
        .replace("\"role\": \"Secondary\"", "\"role\": \"Tertiary\"")
        .replace("\"disk-state\": \"UpToDate\"", "\"disk-state\": \"Sparkling\"");
    let st = one(&json);
    assert_eq!(st.role, Role::Other);
    assert_eq!(st.device().unwrap().disk_state, DiskState::Other);
}
```

Путь в `include_str!` отсчитывается от файла теста: `src/drbd/tests/` на три уровня ниже корня репозитория.

- [ ] **Step 2: подключить модуль и убедиться, что тесты не собираются**

В `src/drbd/mod.rs` добавить строку `pub mod status;`.

`src/drbd/status.rs` — пока только подключение тестов:

```rust
#[cfg(test)]
#[path = "tests/status.rs"]
mod tests;
```

```bash
cargo test 2>&1 | grep -E "^error" | head -3
```

Ожидается ошибка компиляции: `cannot find type ResourceStatus`.

- [ ] **Step 3: написать типы и разбор**

`src/drbd/status.rs` целиком:

```rust
//! Typed view of `drbdsetup status --json`.
//!
//! Only the fields the agent acts on are modelled; unknown keys are ignored
//! and unknown enum values map to `Other`, so a newer DRBD does not break
//! parsing. The JSON mixes kebab-case and snake_case keys (`disk-state`,
//! `peer_devices`); the renames below follow the captured samples in
//! `stand/spike/samples/`.

use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum Role {
    Primary,
    Secondary,
    Unknown,
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum DiskState {
    Diskless,
    Attaching,
    Detaching,
    Failed,
    Negotiating,
    Inconsistent,
    Outdated,
    DUnknown,
    Consistent,
    UpToDate,
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum ConnectionState {
    StandAlone,
    Disconnecting,
    Unconnected,
    Timeout,
    BrokenPipe,
    NetworkFailure,
    ProtocolError,
    TearDown,
    Connecting,
    Connected,
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Device {
    pub volume: u32,
    pub minor: u32,
    #[serde(rename = "disk-state")]
    pub disk_state: DiskState,
    /// `true` on a node that attaches without a disk.
    pub client: bool,
    /// `true` while something holds the device open (a mount, for example).
    pub open: bool,
    pub quorum: bool,
    /// Usable size in KiB; 0 when DRBD does not report it.
    #[serde(default)]
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PeerDevice {
    pub volume: u32,
    /// `Established`, `Off`, `SyncTarget`, `SyncSource` and others.
    #[serde(rename = "replication-state")]
    pub replication_state: String,
    #[serde(rename = "peer-disk-state")]
    pub peer_disk_state: DiskState,
    #[serde(rename = "peer-client")]
    pub peer_client: bool,
    /// KiB that still have to be resynchronised with this peer.
    #[serde(rename = "out-of-sync")]
    pub out_of_sync: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Connection {
    #[serde(rename = "peer-node-id")]
    pub peer_node_id: u32,
    pub name: String,
    #[serde(rename = "connection-state")]
    pub connection_state: ConnectionState,
    #[serde(rename = "peer-role")]
    pub peer_role: Role,
    pub peer_devices: Vec<PeerDevice>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ResourceStatus {
    pub name: String,
    #[serde(rename = "node-id")]
    pub node_id: u32,
    pub role: Role,
    pub suspended: bool,
    pub devices: Vec<Device>,
    pub connections: Vec<Connection>,
}

/// Parses the JSON array printed by `drbdsetup status --json`.
pub fn parse(json: &str) -> Result<Vec<ResourceStatus>, serde_json::Error> {
    serde_json::from_str(json)
}

impl ResourceStatus {
    /// Volume 0: every ambar resource has exactly one volume.
    pub fn device(&self) -> Option<&Device> {
        self.devices.iter().find(|d| d.volume == 0)
    }

    pub fn quorum(&self) -> bool {
        self.device().is_some_and(|d| d.quorum)
    }

    pub fn all_peers_connected(&self) -> bool {
        self.connections
            .iter()
            .all(|c| c.connection_state == ConnectionState::Connected)
    }

    /// Names of the peers whose connection is `StandAlone`.
    pub fn standalone_peers(&self) -> Vec<&str> {
        self.connections
            .iter()
            .filter(|c| c.connection_state == ConnectionState::StandAlone)
            .map(|c| c.name.as_str())
            .collect()
    }

    /// `true` only for a freshly created volume that has never held data:
    /// this node has an `Inconsistent` disk, every peer is connected, and
    /// every peer either has an `Inconsistent` disk too or attaches without
    /// one. Only in this state may the initial full sync be skipped.
    pub fn ready_to_skip_initial_sync(&self) -> bool {
        let local_inconsistent = self.device().is_some_and(|d| d.disk_state == DiskState::Inconsistent);
        let peers_fresh = self.connections.iter().all(|c| {
            c.peer_devices.iter().all(|p| {
                matches!(p.peer_disk_state, DiskState::Inconsistent)
                    || (p.peer_client && p.peer_disk_state == DiskState::Diskless)
            })
        });
        local_inconsistent && !self.connections.is_empty() && self.all_peers_connected() && peers_fresh
    }
}

#[cfg(test)]
#[path = "tests/status.rs"]
mod tests;
```

- [ ] **Step 4: прогнать гейт**

```bash
cargo fmt && cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test 2>&1 | grep -E "^test result" | head -1
```

Ожидается: `test result: ok. 18 passed; 0 failed`.

- [ ] **Step 5: коммит**

```bash
git add src
git commit -m "feat(agent): typed view of drbdsetup status with the stand samples as fixtures" -- src
```

---

### Task 5: запуск команд и загрузчик модуля

**Files:**
- Create: `src/exec.rs`, `src/module_loader.rs`, `src/tests/mod.rs`, `src/tests/module_loader.rs`
- Modify: `src/lib.rs`, `src/main.rs`

**Interfaces:**
- Consumes: ничего из предыдущих задач.
- Produces:
  - `ambar::exec::Output { code: i32, stdout: String, stderr: String }` с методом `success() -> bool`;
  - `ambar::exec::Runner` — трейт с `fn run(&self, program: &str, args: &[&str]) -> io::Result<Output>`; `Err` означает, что процесс не удалось запустить;
  - `ambar::exec::SystemRunner` — запуск настоящих процессов; `ambar::exec::command_line(program, args) -> String`;
  - `ambar::module_loader::LoaderEnv<'a> { modules_dir: &'a Path, sys_module_dir: &'a Path, kernel_release: &'a str }`;
  - `ambar::module_loader::load(env: &LoaderEnv, runner: &dyn Runner) -> Result<String, LoadError>`;
  - `enum LoadError { InTreeLoaded(String), NoModule(String), NoTransport(String), Command { command, code, stderr }, Io(String) }`;
  - для тестов крейта: `crate::tests::FakeRunner::new(reply)`, `FakeRunner::calls() -> Vec<String>`, `crate::tests::out(code, stdout, stderr) -> Output`;
  - подкоманда `ambar load-module [--modules-dir <каталог>]`.

Правила загрузки те же, что в `image/load-module.sh`; сообщения об ошибках совпадают дословно.

- [ ] **Step 1: добавить зависимости**

```bash
cargo add anyhow
cargo add --dev tempfile
```

- [ ] **Step 2: написать запуск команд**

`src/exec.rs`:

```rust
//! Running external commands behind a trait, so the logic is testable.

use std::io;
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    /// Exit code; -1 when the process was killed by a signal.
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Output {
    pub fn success(&self) -> bool {
        self.code == 0
    }
}

pub trait Runner {
    /// Runs a program and waits for it. `Err` means it could not be started.
    fn run(&self, program: &str, args: &[&str]) -> io::Result<Output>;
}

/// Runs real processes.
pub struct SystemRunner;

impl Runner for SystemRunner {
    fn run(&self, program: &str, args: &[&str]) -> io::Result<Output> {
        let out = Command::new(program).args(args).output()?;
        Ok(Output {
            code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }
}

/// Renders a command line for error messages and logs.
pub fn command_line(program: &str, args: &[&str]) -> String {
    let mut line = String::from(program);
    for arg in args {
        line.push(' ');
        line.push_str(arg);
    }
    line
}
```

- [ ] **Step 3: написать тестовый `FakeRunner`**

`src/tests/mod.rs`:

```rust
//! Helpers shared by the unit tests of this crate.

use std::cell::RefCell;
use std::io;

use crate::exec::{Output, Runner, command_line};

type Reply = Box<dyn Fn(&str, &[&str]) -> Output>;

/// Records every command line and answers from a closure.
pub(crate) struct FakeRunner {
    calls: RefCell<Vec<String>>,
    reply: Reply,
}

impl FakeRunner {
    pub(crate) fn new(reply: impl Fn(&str, &[&str]) -> Output + 'static) -> Self {
        Self {
            calls: RefCell::new(Vec::new()),
            reply: Box::new(reply),
        }
    }

    pub(crate) fn calls(&self) -> Vec<String> {
        self.calls.borrow().clone()
    }
}

impl Runner for FakeRunner {
    fn run(&self, program: &str, args: &[&str]) -> io::Result<Output> {
        self.calls.borrow_mut().push(command_line(program, args));
        Ok((self.reply)(program, args))
    }
}

pub(crate) fn out(code: i32, stdout: &str, stderr: &str) -> Output {
    Output {
        code,
        stdout: stdout.to_string(),
        stderr: stderr.to_string(),
    }
}
```

- [ ] **Step 4: написать тесты загрузчика**

`src/tests/module_loader.rs`:

```rust
use std::fs;
use std::path::{Path, PathBuf};

use tempfile::TempDir;

use super::*;
use crate::tests::{FakeRunner, out};

const KVER: &str = "6.12.116+deb13-arm64";

struct Fixture {
    _tmp: TempDir,
    modules: PathBuf,
    sys: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        let modules = tmp.path().join("modules");
        let sys = tmp.path().join("sys");
        fs::create_dir_all(modules.join(KVER)).unwrap();
        fs::create_dir_all(&sys).unwrap();
        Self {
            _tmp: tmp,
            modules,
            sys,
        }
    }

    fn with_ko(self, name: &str) -> Self {
        fs::write(self.modules.join(KVER).join(name), b"").unwrap();
        self
    }

    fn with_loaded(self, module: &str, version: Option<&str>) -> Self {
        mark_loaded(&self.sys, module, version);
        self
    }

    fn env(&self) -> LoaderEnv<'_> {
        LoaderEnv {
            modules_dir: &self.modules,
            sys_module_dir: &self.sys,
            kernel_release: KVER,
        }
    }

    /// A runner whose `insmod` makes the module appear in the fake sysfs,
    /// the way the kernel does.
    fn kernel(&self) -> FakeRunner {
        let sys = self.sys.clone();
        FakeRunner::new(move |program, args| {
            if program == "insmod" {
                if args[0].ends_with("/drbd.ko") {
                    mark_loaded(&sys, "drbd", Some("9.3.4"));
                } else if args[0].ends_with("/drbd_transport_tcp.ko") {
                    mark_loaded(&sys, "drbd_transport_tcp", None);
                }
            }
            out(0, "", "")
        })
    }

    /// Call log with the temporary modules directory replaced by `<dir>`.
    fn calls(&self, runner: &FakeRunner) -> Vec<String> {
        let dir = self.modules.join(KVER).to_string_lossy().into_owned();
        runner.calls().iter().map(|c| c.replace(&dir, "<dir>")).collect()
    }
}

fn mark_loaded(sys: &Path, module: &str, version: Option<&str>) {
    fs::create_dir_all(sys.join(module)).unwrap();
    if let Some(version) = version {
        fs::write(sys.join(module).join("version"), format!("{version}\n")).unwrap();
    }
}

#[test]
fn nothing_loaded_loads_dependencies_core_and_transport_in_order() {
    let fx = Fixture::new().with_ko("drbd.ko").with_ko("drbd_transport_tcp.ko");
    let runner = fx.kernel();
    assert_eq!(
        load(&fx.env(), &runner),
        Ok(format!("loaded drbd 9.3.4 for kernel {KVER}"))
    );
    assert_eq!(
        fx.calls(&runner),
        [
            "modprobe -a lru_cache libcrc32c",
            "insmod <dir>/drbd.ko usermode_helper=disabled",
            "insmod <dir>/drbd_transport_tcp.ko",
        ]
    );
}

#[test]
fn drbd9_with_transport_already_loaded_does_nothing() {
    let fx = Fixture::new()
        .with_ko("drbd.ko")
        .with_ko("drbd_transport_tcp.ko")
        .with_loaded("drbd", Some("9.3.4"))
        .with_loaded("drbd_transport_tcp", None);
    let runner = fx.kernel();
    assert_eq!(load(&fx.env(), &runner), Ok("drbd 9.3.4 is already loaded".to_string()));
    assert!(runner.calls().is_empty());
}

#[test]
fn drbd9_without_transport_loads_only_the_transport() {
    let fx = Fixture::new()
        .with_ko("drbd.ko")
        .with_ko("drbd_transport_tcp.ko")
        .with_loaded("drbd", Some("9.3.4"));
    let runner = fx.kernel();
    assert_eq!(load(&fx.env(), &runner), Ok("drbd 9.3.4 is already loaded".to_string()));
    assert_eq!(fx.calls(&runner), ["insmod <dir>/drbd_transport_tcp.ko"]);
}

#[test]
fn drbd9_without_transport_and_without_its_file_fails() {
    let fx = Fixture::new().with_ko("drbd.ko").with_loaded("drbd", Some("9.3.4"));
    let runner = fx.kernel();
    assert_eq!(load(&fx.env(), &runner), Err(LoadError::NoTransport(KVER.to_string())));
    assert!(runner.calls().is_empty());
}

#[test]
fn the_in_tree_module_is_refused() {
    let fx = Fixture::new()
        .with_ko("drbd.ko")
        .with_ko("drbd_transport_tcp.ko")
        .with_loaded("drbd", Some("8.4.11"));
    let runner = fx.kernel();
    let err = load(&fx.env(), &runner).unwrap_err();
    assert_eq!(err, LoadError::InTreeLoaded("8.4.11".to_string()));
    assert_eq!(err.to_string(), "in-tree drbd 8.4 is loaded (version 8.4.11)");
    assert!(runner.calls().is_empty());
}

#[test]
fn a_kernel_without_a_bundled_module_is_refused() {
    let fx = Fixture::new();
    let runner = fx.kernel();
    let err = load(&fx.env(), &runner).unwrap_err();
    assert_eq!(err.to_string(), format!("no drbd module for kernel {KVER}"));
    assert!(runner.calls().is_empty());
}

#[test]
fn a_failing_insmod_is_reported_with_its_stderr() {
    let fx = Fixture::new().with_ko("drbd.ko").with_ko("drbd_transport_tcp.ko");
    let runner = FakeRunner::new(|program, _| {
        if program == "insmod" {
            out(1, "", "insmod: ERROR: Invalid module format\n")
        } else {
            out(0, "", "")
        }
    });
    let err = load(&fx.env(), &runner).unwrap_err();
    assert!(
        matches!(&err, LoadError::Command { code: 1, stderr, .. } if stderr == "insmod: ERROR: Invalid module format")
    );
}
```

Поддельный `insmod` создаёт каталог модуля в поддельном `sysfs`, как это делает ядро: загрузчик после `insmod` читает версию из `sysfs`.

- [ ] **Step 5: подключить модули и убедиться, что тесты не собираются**

`src/lib.rs` целиком:

```rust
//! ambar: replicated volumes for k3s on top of DRBD 9.

pub mod drbd;
pub mod exec;
pub mod model;
pub mod module_loader;

#[cfg(test)]
mod tests;
```

`src/module_loader.rs` — пока только подключение тестов:

```rust
#[cfg(test)]
#[path = "tests/module_loader.rs"]
mod tests;
```

```bash
cargo test 2>&1 | grep -E "^error" | head -3
```

Ожидается ошибка компиляции: `cannot find function load`.

- [ ] **Step 6: написать загрузчик**

`src/module_loader.rs` целиком:

```rust
//! Loads the DRBD 9 kernel module that ships in the agent image.
//!
//! Same rules as `image/load-module.sh`; the image test `image/test-loader.sh`
//! covers the shell version, the unit tests here cover this one.

use std::fs;
use std::path::Path;

use crate::exec::{Runner, command_line};

pub struct LoaderEnv<'a> {
    /// Directory with one subdirectory per kernel release.
    pub modules_dir: &'a Path,
    /// `/sys/module` on a real node.
    pub sys_module_dir: &'a Path,
    /// Output of `uname -r`.
    pub kernel_release: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LoadError {
    #[error("in-tree drbd 8.4 is loaded (version {0})")]
    InTreeLoaded(String),
    #[error("no drbd module for kernel {0}")]
    NoModule(String),
    #[error("no drbd_transport_tcp module for kernel {0}")]
    NoTransport(String),
    #[error("{command} failed with code {code}: {stderr}")]
    Command { command: String, code: i32, stderr: String },
    #[error("{0}")]
    Io(String),
}

/// Makes sure DRBD 9 and its TCP transport are loaded.
/// Returns a one-line description of what was found or done.
pub fn load(env: &LoaderEnv, runner: &dyn Runner) -> Result<String, LoadError> {
    let dir = env.modules_dir.join(env.kernel_release);
    let version_file = env.sys_module_dir.join("drbd").join("version");

    if env.sys_module_dir.join("drbd").is_dir() {
        let loaded = read_version(&version_file)?;
        if !loaded.starts_with("9.") {
            return Err(LoadError::InTreeLoaded(loaded));
        }
        // A previous run may have died between the two insmod calls:
        // resources cannot connect without the transport.
        load_transport(env, &dir, runner)?;
        return Ok(format!("drbd {loaded} is already loaded"));
    }

    let core = dir.join("drbd.ko");
    if !core.is_file() {
        return Err(LoadError::NoModule(env.kernel_release.to_string()));
    }

    // Dependencies come from the host module tree mounted at /lib/modules.
    run(runner, "modprobe", &["-a", "lru_cache", "libcrc32c"])?;
    run(runner, "insmod", &[&core.to_string_lossy(), "usermode_helper=disabled"])?;
    load_transport(env, &dir, runner)?;
    let loaded = read_version(&version_file)?;
    Ok(format!("loaded drbd {loaded} for kernel {}", env.kernel_release))
}

fn load_transport(env: &LoaderEnv, dir: &Path, runner: &dyn Runner) -> Result<(), LoadError> {
    if env.sys_module_dir.join("drbd_transport_tcp").is_dir() {
        return Ok(());
    }
    let transport = dir.join("drbd_transport_tcp.ko");
    if !transport.is_file() {
        return Err(LoadError::NoTransport(env.kernel_release.to_string()));
    }
    run(runner, "insmod", &[&transport.to_string_lossy()])
}

fn read_version(path: &Path) -> Result<String, LoadError> {
    fs::read_to_string(path)
        .map(|s| s.trim().to_string())
        .map_err(|e| LoadError::Io(format!("cannot read {}: {e}", path.display())))
}

fn run(runner: &dyn Runner, program: &str, args: &[&str]) -> Result<(), LoadError> {
    let command = command_line(program, args);
    let out = runner
        .run(program, args)
        .map_err(|e| LoadError::Io(format!("cannot start {command}: {e}")))?;
    if out.success() {
        return Ok(());
    }
    Err(LoadError::Command {
        command,
        code: out.code,
        stderr: out.stderr.trim().to_string(),
    })
}

#[cfg(test)]
#[path = "tests/module_loader.rs"]
mod tests;
```

- [ ] **Step 7: подкоманда `load-module`**

`src/main.rs` целиком:

```rust
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use ambar::exec::SystemRunner;
use ambar::module_loader::{self, LoaderEnv};

#[derive(Parser)]
#[command(name = "ambar", version, about = "Replicated volumes for k3s on top of DRBD 9")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Load the DRBD 9 kernel module bundled in the image.
    LoadModule {
        #[arg(long, default_value = "/opt/ambar/modules")]
        modules_dir: PathBuf,
    },
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("ambar: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<()> {
    let runner = SystemRunner;
    match cli.command {
        Command::LoadModule { modules_dir } => {
            let kernel_release = read_trimmed(Path::new("/proc/sys/kernel/osrelease"))?;
            let env = LoaderEnv {
                modules_dir: &modules_dir,
                sys_module_dir: Path::new("/sys/module"),
                kernel_release: &kernel_release,
            };
            println!("{}", module_loader::load(&env, &runner)?);
        }
    }
    Ok(())
}

fn read_trimmed(path: &Path) -> Result<String> {
    let text = fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    Ok(text.trim().to_string())
}
```

- [ ] **Step 8: прогнать гейт**

```bash
cargo fmt && cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test 2>&1 | grep -E "^test result" | head -1
```

Ожидается: `test result: ok. 25 passed; 0 failed`.

```bash
cargo run -q -- load-module; echo "rc=$?"
```

На маке ожидается `ambar: cannot read /proc/sys/kernel/osrelease: …` и `rc=1`: файла `/proc` нет.

- [ ] **Step 9: коммит**

```bash
git add Cargo.toml Cargo.lock src
git commit -m "feat(agent): command runner and the DRBD module loader" -- Cargo.toml Cargo.lock src
```

---

### Task 6: файл тома, loop-устройство и вызовы `drbdadm`

Перед началом прочитать в протоколе раздел «Опыты перед агентом (P1–P6)». Если опыты P2 или P3 дали результат, отличный от ожидания в таблице задачи 1, код и тесты этой задачи приводятся к записанному результату, а расхождение называется в сообщении коммита.

**Files:**
- Create: `src/paths.rs`, `src/backing.rs`, `src/drbd/adm.rs`, `src/tests/backing.rs`, `src/drbd/tests/adm.rs`
- Modify: `src/lib.rs`, `src/drbd/mod.rs`

**Interfaces:**
- Consumes: `ambar::exec::{Runner, Output, command_line}`, `ambar::drbd::status::{self, ResourceStatus}`, `crate::tests::{FakeRunner, out}`.
- Produces:
  - `ambar::paths::Paths { volumes_dir, res_dir, dev_dir }` с `Default` (`/var/lib/rancher/ambar/volumes`, `/etc/drbd.d`, `/dev/ambar`) и методами `image(volume)`, `ready_marker(volume)`, `res_file(volume)`, `device_link(volume)`, все возвращают `PathBuf`;
  - `ambar::backing::metadata_bytes(device_bytes: u64, max_peers: u32) -> u64`, `backing_file_bytes(size_bytes: u64, max_peers: u32) -> u64`;
  - `ambar::backing::create_sparse_file(path: &Path, bytes: u64) -> Result<(), BackingError>` — ошибка, если файл уже есть;
  - `find_loop(runner, file) -> Result<Option<String>, BackingError>`, `attach_loop(runner, file) -> Result<String, BackingError>`, `detach_loop(runner, device) -> Result<(), BackingError>`;
  - `link_device(link: &Path, device: &str) -> Result<(), BackingError>`, `link_target(link: &Path) -> Option<String>`;
  - `enum BackingError { Io { action, path, source }, Spawn { command, message }, Command { command, code, stderr } }`;
  - `ambar::drbd::adm::Drbd<'a> { runner: &'a dyn Runner }` с методами `create_md(volume, max_peers)`, `up(volume)`, `down(volume)`, `skip_initial_sync(volume)` (все `Result<(), DrbdError>`), `adjust(volume) -> Result<Output, DrbdError>`, `status(volume) -> Result<Option<ResourceStatus>, DrbdError>`;
  - `enum DrbdError { Spawn { command, message }, Command { command, code, stderr }, Status(String) }`.

- [ ] **Step 1: пути**

`src/paths.rs`:

```rust
//! Where the agent keeps volume files, DRBD resource files and device links.

use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    /// Sparse backing files, one per volume with a copy on this node.
    pub volumes_dir: PathBuf,
    /// DRBD resource files; `/etc/drbd.conf` includes `*.res` from here.
    pub res_dir: PathBuf,
    /// Stable names for the loop devices: `<dev_dir>/<volume>` -> `/dev/loopN`.
    pub dev_dir: PathBuf,
}

impl Default for Paths {
    fn default() -> Self {
        Self {
            volumes_dir: PathBuf::from("/var/lib/rancher/ambar/volumes"),
            res_dir: PathBuf::from("/etc/drbd.d"),
            dev_dir: PathBuf::from("/dev/ambar"),
        }
    }
}

impl Paths {
    /// Sparse backing file of the copy on this node.
    pub fn image(&self, volume: &str) -> PathBuf {
        self.volumes_dir.join(format!("{volume}.img"))
    }

    /// Empty marker written after DRBD metadata was created on the backing
    /// file. A backing file without it has to be initialised again.
    pub fn ready_marker(&self, volume: &str) -> PathBuf {
        self.volumes_dir.join(format!("{volume}.img.ready"))
    }

    pub fn res_file(&self, volume: &str) -> PathBuf {
        self.res_dir.join(format!("{volume}.res"))
    }

    pub fn device_link(&self, volume: &str) -> PathBuf {
        self.dev_dir.join(volume)
    }
}
```

Маркер `<том>.img.ready` создаётся после `create-md`. Файл тома без маркера считается неинициализированным: метаданные создаются заново. Файл с маркером `create-md` не получает никогда.

- [ ] **Step 2: тесты файла и loop-устройства**

`src/tests/backing.rs`:

```rust
use std::fs;
use std::os::unix::fs::MetadataExt;

use tempfile::TempDir;

use super::*;
use crate::tests::{FakeRunner, out};

const KIB: u64 = 1024;
const GIB: u64 = 1024 * 1024 * 1024;

#[test]
fn metadata_size_matches_the_stand_measurements() {
    // A 1 GiB backing file gave a 1048476 KiB device with two peers
    // and a 1048508 KiB device with one peer.
    assert_eq!(metadata_bytes(GIB, 2), 100 * KIB);
    assert_eq!(metadata_bytes(GIB, 1), 68 * KIB);
    assert_eq!((GIB - metadata_bytes(GIB, 2)) / KIB, 1_048_476);
    assert_eq!((GIB - metadata_bytes(GIB, 1)) / KIB, 1_048_508);
}

#[test]
fn the_backing_file_leaves_the_requested_size_usable() {
    for (size, peers) in [(GIB, 2), (GIB, 7), (10 * GIB, 7), (4096, 31), (3 * GIB + 4096, 3)] {
        let file = backing_file_bytes(size, peers);
        assert_eq!(file % 4096, 0);
        let usable = file - metadata_bytes(file, peers);
        assert!(usable >= size, "size {size} peers {peers}: usable {usable}");
        assert!(
            usable - size < 2 * 4096 + 8 * 512 * u64::from(peers),
            "size {size} peers {peers}: wastes {}",
            usable - size
        );
    }
}

#[test]
fn creates_a_sparse_file_once() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("volumes").join("spike.img.new");
    create_sparse_file(&path, 64 * 1024 * 1024).unwrap();
    let meta = fs::metadata(&path).unwrap();
    assert_eq!(meta.len(), 64 * 1024 * 1024);
    assert!(meta.blocks() * 512 < meta.len(), "file is not sparse");
    assert!(matches!(
        create_sparse_file(&path, 4096),
        Err(BackingError::Io { action: "create", .. })
    ));
}

#[test]
fn finds_the_loop_device_of_a_file() {
    let runner = FakeRunner::new(|_, _| {
        out(
            0,
            "/dev/loop3: [0048]:131 (/var/lib/rancher/ambar/volumes/spike.img)\n",
            "",
        )
    });
    let found = find_loop(&runner, Path::new("/var/lib/rancher/ambar/volumes/spike.img")).unwrap();
    assert_eq!(found.as_deref(), Some("/dev/loop3"));
    assert_eq!(runner.calls(), ["losetup -j /var/lib/rancher/ambar/volumes/spike.img"]);

    let none = FakeRunner::new(|_, _| out(0, "", ""));
    assert_eq!(find_loop(&none, Path::new("/x.img")).unwrap(), None);
}

#[test]
fn attaches_with_direct_io_and_detaches() {
    let runner = FakeRunner::new(|_, args| {
        if args[0] == "--find" {
            out(0, "/dev/loop7\n", "")
        } else {
            out(0, "", "")
        }
    });
    assert_eq!(
        attach_loop(&runner, Path::new("/v/spike.img.new")).unwrap(),
        "/dev/loop7"
    );
    detach_loop(&runner, "/dev/loop7").unwrap();
    assert_eq!(
        runner.calls(),
        [
            "losetup --find --show --direct-io=on /v/spike.img.new",
            "losetup -d /dev/loop7"
        ]
    );
}

#[test]
fn a_failing_losetup_is_an_error() {
    let runner = FakeRunner::new(|_, _| out(1, "", "losetup: cannot find an unused loop device\n"));
    let err = attach_loop(&runner, Path::new("/v/spike.img")).unwrap_err();
    assert!(matches!(err, BackingError::Command { code: 1, .. }), "{err}");
}

#[test]
fn links_and_relinks_the_device() {
    let tmp = TempDir::new().unwrap();
    let link = tmp.path().join("ambar").join("spike");
    assert_eq!(link_target(&link), None);
    link_device(&link, "/dev/loop3").unwrap();
    assert_eq!(link_target(&link).as_deref(), Some("/dev/loop3"));
    link_device(&link, "/dev/loop5").unwrap();
    assert_eq!(link_target(&link).as_deref(), Some("/dev/loop5"));
}
```

Первый тест закрепляет два замера стенда: файл 1 ГиБ дал устройство 1048476 КиБ при двух остальных сторонах и 1048508 КиБ при одной.

- [ ] **Step 3: тесты вызовов `drbdadm`**

`src/drbd/tests/adm.rs`:

```rust
use super::*;
use crate::tests::{FakeRunner, out};

const THREE_UPTODATE: &str = include_str!("../../../stand/spike/samples/three-uptodate.json");

#[test]
fn commands_are_built_as_recorded_on_the_stand() {
    let runner = FakeRunner::new(|_, _| out(0, "", ""));
    let drbd = Drbd { runner: &runner };
    drbd.create_md("spike", 7).unwrap();
    drbd.up("spike").unwrap();
    drbd.adjust("spike").unwrap();
    drbd.skip_initial_sync("spike").unwrap();
    drbd.down("spike").unwrap();
    assert_eq!(
        runner.calls(),
        [
            "drbdadm create-md --force --max-peers=7 spike",
            "drbdadm up spike",
            "drbdadm adjust spike",
            "drbdadm new-current-uuid --clear-bitmap spike/0",
            "drbdadm down spike",
        ]
    );
}

#[test]
fn a_failing_command_reports_its_code_and_stderr() {
    let runner = FakeRunner::new(|_, _| out(10, "", "No valid meta data found\n"));
    let drbd = Drbd { runner: &runner };
    assert_eq!(
        drbd.up("spike"),
        Err(DrbdError::Command {
            command: "drbdadm up spike".into(),
            code: 10,
            stderr: "No valid meta data found".into(),
        })
    );
}

#[test]
fn adjust_hands_back_the_output_instead_of_judging_the_code() {
    let runner = FakeRunner::new(|_, _| out(1, "", "whatever"));
    let drbd = Drbd { runner: &runner };
    assert_eq!(drbd.adjust("spike").unwrap().code, 1);
}

#[test]
fn status_finds_the_resource_by_name() {
    let runner = FakeRunner::new(|_, _| out(0, THREE_UPTODATE, ""));
    let drbd = Drbd { runner: &runner };
    assert_eq!(drbd.status("spike").unwrap().map(|s| s.node_id), Some(0));
    assert_eq!(drbd.status("other").unwrap(), None);
    assert_eq!(runner.calls()[0], "drbdsetup status --json");
}

#[test]
fn status_without_configured_resources_is_none() {
    let runner = FakeRunner::new(|_, _| out(0, "# No currently configured DRBD found.\n", ""));
    let drbd = Drbd { runner: &runner };
    assert_eq!(drbd.status("spike").unwrap(), None);
}

#[test]
fn status_fails_when_drbdsetup_fails() {
    let runner = FakeRunner::new(|_, _| out(20, "", "modprobe: FATAL: Module drbd not found\n"));
    let drbd = Drbd { runner: &runner };
    assert!(matches!(drbd.status("spike"), Err(DrbdError::Command { code: 20, .. })));
}
```

- [ ] **Step 4: подключить модули и убедиться, что тесты не собираются**

`src/lib.rs` целиком:

```rust
//! ambar: replicated volumes for k3s on top of DRBD 9.

pub mod backing;
pub mod drbd;
pub mod exec;
pub mod model;
pub mod module_loader;
pub mod paths;

#[cfg(test)]
mod tests;
```

`src/drbd/mod.rs` целиком:

```rust
//! Everything that talks to or about DRBD.

pub mod adm;
pub mod resource;
pub mod status;
```

`src/backing.rs` и `src/drbd/adm.rs` — пока только подключение тестов (`#[path = "tests/backing.rs"]` и `#[path = "tests/adm.rs"]` соответственно):

```rust
#[cfg(test)]
#[path = "tests/backing.rs"]
mod tests;
```

```bash
cargo test 2>&1 | grep -E "^error" | head -3
```

Ожидается ошибка компиляции: `cannot find function metadata_bytes`.

- [ ] **Step 5: файл и loop-устройство**

`src/backing.rs` целиком:

```rust
//! The backing store of a copy: a sparse file attached as a loop device.

use std::fs::{self, OpenOptions};
use std::io;
use std::path::Path;

use crate::exec::{Runner, command_line};

const SECTOR: u64 = 512;
/// One bitmap extent covers 2^18 sectors.
const SECTORS_PER_EXTENT: u64 = 1 << 18;

#[derive(Debug, thiserror::Error)]
pub enum BackingError {
    #[error("{action} {path}: {source}")]
    Io {
        action: &'static str,
        path: String,
        source: io::Error,
    },
    #[error("cannot start {command}: {message}")]
    Spawn { command: String, message: String },
    #[error("{command} failed with code {code}: {stderr}")]
    Command { command: String, code: i32, stderr: String },
}

/// Size of DRBD's internal metadata on a backing device of `device_bytes`
/// for `max_peers` peers: 8 sectors of bitmap per extent and peer, plus 72
/// sectors of fixed overhead. Matches the stand: a 1 GiB file lost 100 KiB
/// with two peers and 68 KiB with one.
pub fn metadata_bytes(device_bytes: u64, max_peers: u32) -> u64 {
    let extents = device_bytes.div_ceil(SECTOR).div_ceil(SECTORS_PER_EXTENT);
    (extents * 8 * u64::from(max_peers) + 72) * SECTOR
}

/// Size of the backing file that leaves at least `size_bytes` usable after
/// the metadata is taken off its end. Rounded up to 4 KiB.
pub fn backing_file_bytes(size_bytes: u64, max_peers: u32) -> u64 {
    let mut file = size_bytes;
    loop {
        let needed = (size_bytes + metadata_bytes(file, max_peers)).next_multiple_of(4096);
        if needed == file {
            return file;
        }
        file = needed;
    }
}

/// Creates a sparse file of `bytes`. Fails if the file already exists.
pub fn create_sparse_file(path: &Path, bytes: u64) -> Result<(), BackingError> {
    let io_err = |action, source| BackingError::Io {
        action,
        path: path.display().to_string(),
        source,
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| io_err("create directory for", e))?;
    }
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| io_err("create", e))?;
    file.set_len(bytes).map_err(|e| io_err("resize", e))
}

/// The loop device a file is attached to, if any.
pub fn find_loop(runner: &dyn Runner, file: &Path) -> Result<Option<String>, BackingError> {
    let path = file.to_string_lossy();
    let out = checked(runner, "losetup", &["-j", &path])?;
    // One line per device: "/dev/loop0: [0048]:1234 (/path/to/file)".
    Ok(out
        .lines()
        .next()
        .and_then(|line| line.split(':').next())
        .map(str::to_string)
        .filter(|d| !d.is_empty()))
}

/// Attaches a file to a free loop device with direct I/O; returns the device.
pub fn attach_loop(runner: &dyn Runner, file: &Path) -> Result<String, BackingError> {
    let path = file.to_string_lossy();
    let out = checked(runner, "losetup", &["--find", "--show", "--direct-io=on", &path])?;
    Ok(out.trim().to_string())
}

pub fn detach_loop(runner: &dyn Runner, device: &str) -> Result<(), BackingError> {
    checked(runner, "losetup", &["-d", device]).map(|_| ())
}

/// Points `link` at `device`, replacing whatever was there.
pub fn link_device(link: &Path, device: &str) -> Result<(), BackingError> {
    let io_err = |action, source| BackingError::Io {
        action,
        path: link.display().to_string(),
        source,
    };
    if let Some(parent) = link.parent() {
        fs::create_dir_all(parent).map_err(|e| io_err("create directory for", e))?;
    }
    match fs::remove_file(link) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(io_err("remove", e)),
    }
    std::os::unix::fs::symlink(device, link).map_err(|e| io_err("link", e))
}

/// Where `link` points, or `None` when it does not exist.
pub fn link_target(link: &Path) -> Option<String> {
    fs::read_link(link).ok().map(|p| p.to_string_lossy().into_owned())
}

fn checked(runner: &dyn Runner, program: &str, args: &[&str]) -> Result<String, BackingError> {
    let command = command_line(program, args);
    let out = runner.run(program, args).map_err(|e| BackingError::Spawn {
        command: command.clone(),
        message: e.to_string(),
    })?;
    if out.success() {
        return Ok(out.stdout);
    }
    Err(BackingError::Command {
        command,
        code: out.code,
        stderr: out.stderr.trim().to_string(),
    })
}

#[cfg(test)]
#[path = "tests/backing.rs"]
mod tests;
```

- [ ] **Step 6: вызовы `drbdadm` и `drbdsetup`**

`src/drbd/adm.rs` целиком:

```rust
//! Thin wrappers around `drbdadm` and `drbdsetup`.
//!
//! Return codes of `drbdadm` do not reliably tell whether an operation took
//! effect (see the experiment record, C2 and C3): callers re-read the status
//! after a state-changing command instead of trusting the code. These
//! wrappers therefore fail only when a command clearly did not run.

use crate::exec::{Output, Runner, command_line};

use super::status::{self, ResourceStatus};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DrbdError {
    #[error("cannot start {command}: {message}")]
    Spawn { command: String, message: String },
    #[error("{command} failed with code {code}: {stderr}")]
    Command { command: String, code: i32, stderr: String },
    #[error("cannot parse drbdsetup status: {0}")]
    Status(String),
}

pub struct Drbd<'a> {
    pub runner: &'a dyn Runner,
}

impl Drbd<'_> {
    /// Writes fresh metadata onto the backing device of `volume`.
    /// Destroys existing metadata: call only for a backing file that was
    /// just created.
    pub fn create_md(&self, volume: &str, max_peers: u32) -> Result<(), DrbdError> {
        let peers = format!("--max-peers={max_peers}");
        self.checked("drbdadm", &["create-md", "--force", &peers, volume])
    }

    /// Configures a resource that is not configured in the kernel yet.
    pub fn up(&self, volume: &str) -> Result<(), DrbdError> {
        self.checked("drbdadm", &["up", volume])
    }

    /// Brings an already configured resource in line with its resource file.
    /// The return code is not a verdict: re-read the status afterwards.
    pub fn adjust(&self, volume: &str) -> Result<Output, DrbdError> {
        self.run("drbdadm", &["adjust", volume])
    }

    pub fn down(&self, volume: &str) -> Result<(), DrbdError> {
        self.checked("drbdadm", &["down", volume])
    }

    /// Declares all copies of a brand-new volume identical, so DRBD does not
    /// copy an empty device across the network. Run on one node only, and
    /// only when `ResourceStatus::ready_to_skip_initial_sync` holds.
    pub fn skip_initial_sync(&self, volume: &str) -> Result<(), DrbdError> {
        let target = format!("{volume}/0");
        self.checked("drbdadm", &["new-current-uuid", "--clear-bitmap", &target])
    }

    /// Status of one resource; `None` when it is not configured in the kernel.
    pub fn status(&self, volume: &str) -> Result<Option<ResourceStatus>, DrbdError> {
        let args = ["status", "--json"];
        let out = self.run("drbdsetup", &args)?;
        let text = out.stdout.trim();
        if !text.starts_with('[') {
            // Without any configured resource drbdsetup prints a comment
            // line instead of JSON.
            if out.success() {
                return Ok(None);
            }
            return Err(DrbdError::Command {
                command: command_line("drbdsetup", &args),
                code: out.code,
                stderr: out.stderr.trim().to_string(),
            });
        }
        let all = status::parse(text).map_err(|e| DrbdError::Status(e.to_string()))?;
        Ok(all.into_iter().find(|r| r.name == volume))
    }

    fn run(&self, program: &str, args: &[&str]) -> Result<Output, DrbdError> {
        self.runner.run(program, args).map_err(|e| DrbdError::Spawn {
            command: command_line(program, args),
            message: e.to_string(),
        })
    }

    fn checked(&self, program: &str, args: &[&str]) -> Result<(), DrbdError> {
        let out = self.run(program, args)?;
        if out.success() {
            return Ok(());
        }
        Err(DrbdError::Command {
            command: command_line(program, args),
            code: out.code,
            stderr: out.stderr.trim().to_string(),
        })
    }
}

#[cfg(test)]
#[path = "tests/adm.rs"]
mod tests;
```

- [ ] **Step 7: прогнать гейт**

```bash
cargo fmt && cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test 2>&1 | grep -E "^test result" | head -1
```

Ожидается: `test result: ok. 38 passed; 0 failed`.

- [ ] **Step 8: коммит**

```bash
git add src
git commit -m "feat(agent): backing file, loop device and drbdadm wrappers" -- src
```

---

### Task 7: согласование тома и CLI

**Files:**
- Create: `src/reconcile.rs`, `src/tests/reconcile.rs`
- Modify: `src/lib.rs`, `src/main.rs`

**Interfaces:**
- Consumes: `VolumeSpec` и `initial_sync_leader` (задача 2), `resource::render` (задача 3), `ResourceStatus` и `ready_to_skip_initial_sync` (задача 4), `Runner` и `FakeRunner` (задача 5), `Paths`, `backing::*`, `Drbd` (задача 6).
- Produces (`ambar::reconcile`):
  - `enum FileState { Absent, Pending, Ready }`;
  - `struct Observed { file: FileState, loop_device: Option<String>, link_target: Option<String>, res_matches: bool, status: Option<ResourceStatus> }`;
  - `enum Action { CreateFile { bytes: u64 }, AttachLoop, LinkDevice, WriteRes, CreateMd, MarkReady, Up, Adjust, SkipInitialSync }`;
  - `plan(spec: &VolumeSpec, node_name: &str, obs: &Observed) -> Result<Vec<Action>, PlanError>`;
  - `enum PlanError { NotAMember { node, volume }, UninitialisedFileWithLiveResource { volume } }`;
  - `struct Reconciler<'a> { paths: &'a Paths, runner: &'a dyn Runner }` с `apply(spec, node_name) -> Result<Vec<Action>, ReconcileError>`, `observe(spec, node_name) -> Result<Observed, ReconcileError>`, `remove(volume) -> Result<Vec<&'static str>, ReconcileError>`;
  - подкоманды `ambar apply --file <json> [--node <имя>]`, `ambar remove --name <том>`, `ambar status --name <том>`.

Правила `plan`:

| Условие | Действия |
|---|---|
| узел с копией, файла нет | `CreateFile`, `AttachLoop`, `LinkDevice`, `WriteRes`, `CreateMd`, `MarkReady`, `Up` |
| файл с маркером, loop-устройства нет, ресурс не поднят (после перезагрузки) | `AttachLoop`, `LinkDevice`, `Up` |
| файл без маркера, ресурс не поднят (прерванная инициализация) | недостающие шаги, затем `CreateMd`, `MarkReady`, `Up` |
| файл без маркера, ресурс поднят | ошибка `UninitialisedFileWithLiveResource` |
| узел без диска, ресурс не поднят | `WriteRes` при необходимости, `Up` |
| файл ресурса отличается, ресурс поднят | `WriteRes`, `Adjust` |
| всё на месте, узел — ведущий, статус готов к пропуску синхронизации | `SkipInitialSync` |
| всё на месте | пусто |

`apply` делает не больше двух проходов: первый создаёт и поднимает, второй может пропустить начальную синхронизацию. Третий проход повторил бы эту команду.

- [ ] **Step 1: написать тесты**

`src/tests/reconcile.rs`:

```rust
use std::cell::Cell;
use std::fs;
use std::rc::Rc;

use tempfile::TempDir;

use super::*;
use crate::backing::backing_file_bytes;
use crate::drbd::status;
use crate::tests::{FakeRunner, out};

const SPEC: &str = r#"{
  "name": "spike",
  "sizeBytes": 1073741824,
  "protocol": "C",
  "minor": 1000,
  "port": 7000,
  "maxPeers": 7,
  "nodes": [
    {"name": "lima-dv-1", "address": "192.168.104.1", "nodeId": 0, "diskful": true},
    {"name": "lima-dv-2", "address": "192.168.104.3", "nodeId": 1, "diskful": true},
    {"name": "lima-dv-3", "address": "192.168.104.4", "nodeId": 2, "diskful": false}
  ]
}"#;

const THREE_INCONSISTENT: &str = include_str!("../../stand/spike/samples/three-inconsistent.json");
const THREE_UPTODATE: &str = include_str!("../../stand/spike/samples/three-uptodate.json");

fn spec() -> VolumeSpec {
    VolumeSpec::from_json(SPEC).unwrap()
}

fn status_of(json: &str) -> Option<ResourceStatus> {
    status::parse(json).unwrap().into_iter().next()
}

fn steady() -> Observed {
    Observed {
        file: FileState::Ready,
        loop_device: Some("/dev/loop3".into()),
        link_target: Some("/dev/loop3".into()),
        res_matches: true,
        status: status_of(THREE_UPTODATE),
    }
}

#[test]
fn a_new_copy_is_created_initialised_and_brought_up() {
    let obs = Observed {
        file: FileState::Absent,
        loop_device: None,
        link_target: None,
        res_matches: false,
        status: None,
    };
    let bytes = backing_file_bytes(1_073_741_824, 7);
    assert_eq!(
        plan(&spec(), "lima-dv-1", &obs).unwrap(),
        [
            Action::CreateFile { bytes },
            Action::AttachLoop,
            Action::LinkDevice,
            Action::WriteRes,
            Action::CreateMd,
            Action::MarkReady,
            Action::Up,
        ]
    );
}

#[test]
fn after_a_reboot_the_loop_device_is_restored_without_touching_metadata() {
    let obs = Observed {
        loop_device: None,
        link_target: None,
        status: None,
        ..steady()
    };
    assert_eq!(
        plan(&spec(), "lima-dv-1", &obs).unwrap(),
        [Action::AttachLoop, Action::LinkDevice, Action::Up]
    );
}

#[test]
fn an_interrupted_initialisation_is_finished() {
    let obs = Observed {
        file: FileState::Pending,
        status: None,
        ..steady()
    };
    assert_eq!(
        plan(&spec(), "lima-dv-1", &obs).unwrap(),
        [Action::CreateMd, Action::MarkReady, Action::Up]
    );
}

#[test]
fn metadata_is_never_created_under_a_configured_resource() {
    let obs = Observed {
        file: FileState::Pending,
        ..steady()
    };
    assert_eq!(
        plan(&spec(), "lima-dv-1", &obs),
        Err(PlanError::UninitialisedFileWithLiveResource { volume: "spike".into() })
    );
}

#[test]
fn a_node_without_a_disk_only_needs_the_resource() {
    let obs = Observed {
        file: FileState::Absent,
        loop_device: None,
        link_target: None,
        res_matches: false,
        status: None,
    };
    assert_eq!(
        plan(&spec(), "lima-dv-3", &obs).unwrap(),
        [Action::WriteRes, Action::Up]
    );
}

#[test]
fn a_changed_description_is_applied_with_adjust() {
    let obs = Observed {
        res_matches: false,
        ..steady()
    };
    assert_eq!(
        plan(&spec(), "lima-dv-1", &obs).unwrap(),
        [Action::WriteRes, Action::Adjust]
    );
    assert_eq!(
        plan(&spec(), "lima-dv-3", &obs).unwrap(),
        [Action::WriteRes, Action::Adjust]
    );
}

#[test]
fn a_stale_device_link_is_repointed() {
    let obs = Observed {
        link_target: Some("/dev/loop9".into()),
        ..steady()
    };
    assert_eq!(plan(&spec(), "lima-dv-1", &obs).unwrap(), [Action::LinkDevice]);
}

#[test]
fn a_settled_volume_needs_nothing() {
    assert_eq!(plan(&spec(), "lima-dv-1", &steady()).unwrap(), []);
    assert_eq!(plan(&spec(), "lima-dv-3", &steady()).unwrap(), []);
}

#[test]
fn only_the_lowest_diskful_node_skips_the_initial_sync() {
    let obs = Observed {
        status: status_of(THREE_INCONSISTENT),
        ..steady()
    };
    assert_eq!(plan(&spec(), "lima-dv-1", &obs).unwrap(), [Action::SkipInitialSync]);
    assert_eq!(plan(&spec(), "lima-dv-2", &obs).unwrap(), []);
    assert_eq!(plan(&spec(), "lima-dv-3", &obs).unwrap(), []);
}

#[test]
fn a_foreign_node_is_rejected() {
    assert_eq!(
        plan(&spec(), "elsewhere", &steady()),
        Err(PlanError::NotAMember {
            node: "elsewhere".into(),
            volume: "spike".into()
        })
    );
}

/// A fake node: `losetup` remembers the attachment, `drbdadm up` makes the
/// resource appear in `drbdsetup status`.
fn fake_node() -> FakeRunner {
    let attached = Rc::new(Cell::new(false));
    let up = Rc::new(Cell::new(false));
    FakeRunner::new(move |program, args| match (program, args) {
        ("losetup", ["-j", _]) if attached.get() => out(0, "/dev/loop7: [0048]:131 (image)\n", ""),
        ("losetup", ["--find", ..]) => {
            attached.set(true);
            out(0, "/dev/loop7\n", "")
        }
        ("losetup", ["-d", _]) => {
            attached.set(false);
            out(0, "", "")
        }
        ("drbdadm", ["up", _]) => {
            up.set(true);
            out(0, "", "")
        }
        ("drbdadm", ["down", _]) => {
            up.set(false);
            out(0, "", "")
        }
        ("drbdsetup", _) if up.get() => out(0, THREE_INCONSISTENT, ""),
        ("drbdsetup", _) => out(0, "# No currently configured DRBD found.\n", ""),
        _ => out(0, "", ""),
    })
}

fn temp_paths(tmp: &TempDir) -> Paths {
    Paths {
        volumes_dir: tmp.path().join("volumes"),
        res_dir: tmp.path().join("res"),
        dev_dir: tmp.path().join("dev"),
    }
}

#[test]
fn apply_creates_a_copy_then_skips_the_initial_sync_and_remove_undoes_it() {
    let tmp = TempDir::new().unwrap();
    let paths = temp_paths(&tmp);
    let runner = fake_node();
    let reconciler = Reconciler {
        paths: &paths,
        runner: &runner,
    };
    let spec = spec();

    let done = reconciler.apply(&spec, "lima-dv-1").unwrap();
    assert_eq!(done.last(), Some(&Action::SkipInitialSync));
    assert_eq!(done.len(), 8);

    let image = paths.image("spike");
    assert_eq!(
        fs::metadata(&image).unwrap().len(),
        backing_file_bytes(spec.size_bytes, spec.max_peers)
    );
    assert!(paths.ready_marker("spike").exists());
    assert_eq!(
        backing::link_target(&paths.device_link("spike")).as_deref(),
        Some("/dev/loop7")
    );
    let res = fs::read_to_string(paths.res_file("spike")).unwrap();
    assert!(
        res.contains(&format!("disk {};", paths.device_link("spike").display())),
        "{res}"
    );

    let image_arg = image.to_string_lossy().into_owned();
    let commands: Vec<String> = runner
        .calls()
        .into_iter()
        .filter(|c| !c.starts_with("drbdsetup") && !c.starts_with("losetup -j"))
        .map(|c| c.replace(&image_arg, "<image>"))
        .collect();
    assert_eq!(
        commands,
        [
            "losetup --find --show --direct-io=on <image>",
            "drbdadm create-md --force --max-peers=7 spike",
            "drbdadm up spike",
            "drbdadm new-current-uuid --clear-bitmap spike/0",
        ]
    );

    // A second apply right away has nothing new to create.
    let again = reconciler.apply(&spec, "lima-dv-1").unwrap();
    assert!(
        !again
            .iter()
            .any(|a| matches!(a, Action::CreateFile { .. } | Action::CreateMd | Action::Up)),
        "{again:?}"
    );

    let steps = reconciler.remove("spike").unwrap();
    assert_eq!(
        steps,
        [
            "down",
            "detach-loop",
            "remove-link",
            "remove-res",
            "remove-marker",
            "remove-file"
        ]
    );
    assert!(!image.exists());
    assert_eq!(reconciler.remove("spike").unwrap(), Vec::<&str>::new());
}
```

Последний тест прогоняет исполнитель на поддельном узле: `losetup` помнит подключение, `drbdadm up` делает ресурс видимым в `drbdsetup status`.

- [ ] **Step 2: подключить модуль и убедиться, что тесты не собираются**

В `src/lib.rs` добавить строку `pub mod reconcile;` после `pub mod paths;`.

`src/reconcile.rs` — пока только подключение тестов:

```rust
#[cfg(test)]
#[path = "tests/reconcile.rs"]
mod tests;
```

```bash
cargo test 2>&1 | grep -E "^error" | head -3
```

Ожидается ошибка компиляции: `cannot find type Observed`.

- [ ] **Step 3: написать согласование**

`src/reconcile.rs` целиком:

```rust
//! Brings one volume on one node to the state its description asks for.
//!
//! `plan` is a pure function from the description and the observed state to
//! a list of actions; `Reconciler` observes the node and executes the plan.

use std::fs;
use std::io;
use std::path::Path;

use crate::backing::{self, BackingError};
use crate::drbd::adm::{Drbd, DrbdError};
use crate::drbd::resource;
use crate::drbd::status::ResourceStatus;
use crate::exec::Runner;
use crate::model::VolumeSpec;
use crate::paths::Paths;

/// One pass creates and configures; a second pass can skip the initial sync
/// once the peers are connected. More passes would repeat that command.
const MAX_ROUNDS: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileState {
    /// No backing file.
    Absent,
    /// Backing file without the "metadata created" marker.
    Pending,
    /// Backing file with DRBD metadata.
    Ready,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observed {
    pub file: FileState,
    /// Loop device the backing file is attached to.
    pub loop_device: Option<String>,
    /// Target of the stable device link.
    pub link_target: Option<String>,
    /// The resource file on disk equals the rendered one.
    pub res_matches: bool,
    /// `None`: the resource is not configured in the kernel.
    pub status: Option<ResourceStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    CreateFile { bytes: u64 },
    AttachLoop,
    LinkDevice,
    WriteRes,
    CreateMd,
    MarkReady,
    Up,
    Adjust,
    SkipInitialSync,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlanError {
    #[error("node {node:?} is not a member of volume {volume:?}")]
    NotAMember { node: String, volume: String },
    #[error("volume {volume:?}: the backing file has no metadata marker, but the resource is configured in the kernel")]
    UninitialisedFileWithLiveResource { volume: String },
}

/// Decides what has to be done on `node_name` for `spec`, given `obs`.
pub fn plan(spec: &VolumeSpec, node_name: &str, obs: &Observed) -> Result<Vec<Action>, PlanError> {
    let node = spec.node(node_name).ok_or_else(|| PlanError::NotAMember {
        node: node_name.to_string(),
        volume: spec.name.clone(),
    })?;
    let mut actions = Vec::new();

    if node.diskful {
        if obs.file == FileState::Absent {
            let bytes = backing::backing_file_bytes(spec.size_bytes, spec.max_peers);
            actions.push(Action::CreateFile { bytes });
        }
        if obs.loop_device.is_none() {
            actions.push(Action::AttachLoop);
        }
        if obs.loop_device.is_none() || obs.link_target != obs.loop_device {
            actions.push(Action::LinkDevice);
        }
    }
    if !obs.res_matches {
        actions.push(Action::WriteRes);
    }
    if node.diskful && obs.file != FileState::Ready {
        if obs.status.is_some() {
            return Err(PlanError::UninitialisedFileWithLiveResource {
                volume: spec.name.clone(),
            });
        }
        // Metadata is created only for a file this agent has not finished
        // initialising; a ready file is never touched.
        actions.push(Action::CreateMd);
        actions.push(Action::MarkReady);
    }

    match &obs.status {
        None => actions.push(Action::Up),
        Some(_) if !obs.res_matches => actions.push(Action::Adjust),
        Some(status) => {
            let leader = spec.initial_sync_leader().is_some_and(|l| l.name == node_name);
            if actions.is_empty() && leader && status.ready_to_skip_initial_sync() {
                actions.push(Action::SkipInitialSync);
            }
        }
    }
    Ok(actions)
}

#[derive(Debug, thiserror::Error)]
pub enum ReconcileError {
    #[error(transparent)]
    Plan(#[from] PlanError),
    #[error(transparent)]
    Backing(#[from] BackingError),
    #[error(transparent)]
    Drbd(#[from] DrbdError),
    #[error("{action} {path}: {source}")]
    Io {
        action: &'static str,
        path: String,
        source: io::Error,
    },
}

pub struct Reconciler<'a> {
    pub paths: &'a Paths,
    pub runner: &'a dyn Runner,
}

impl Reconciler<'_> {
    /// Observes the node, executes the plan and returns what was done.
    pub fn apply(&self, spec: &VolumeSpec, node_name: &str) -> Result<Vec<Action>, ReconcileError> {
        let mut done = Vec::new();
        for _ in 0..MAX_ROUNDS {
            let obs = self.observe(spec, node_name)?;
            let actions = plan(spec, node_name, &obs)?;
            if actions.is_empty() {
                break;
            }
            let mut loop_device = obs.loop_device.clone();
            for action in &actions {
                self.execute(spec, action, &mut loop_device)?;
            }
            done.extend(actions);
        }
        Ok(done)
    }

    pub fn observe(&self, spec: &VolumeSpec, node_name: &str) -> Result<Observed, ReconcileError> {
        let volume = spec.name.as_str();
        let diskful = spec.node(node_name).is_some_and(|n| n.diskful);
        let image = self.paths.image(volume);

        let file = if !image.exists() {
            FileState::Absent
        } else if self.paths.ready_marker(volume).exists() {
            FileState::Ready
        } else {
            FileState::Pending
        };
        let loop_device = if diskful && file != FileState::Absent {
            backing::find_loop(self.runner, &image)?
        } else {
            None
        };
        let rendered = resource::render(spec, &self.paths.device_link(volume));
        let res_matches = fs::read_to_string(self.paths.res_file(volume)).is_ok_and(|on_disk| on_disk == rendered);
        let status = Drbd { runner: self.runner }.status(volume)?;

        Ok(Observed {
            file,
            loop_device,
            link_target: backing::link_target(&self.paths.device_link(volume)),
            res_matches,
            status,
        })
    }

    fn execute(
        &self,
        spec: &VolumeSpec,
        action: &Action,
        loop_device: &mut Option<String>,
    ) -> Result<(), ReconcileError> {
        let volume = spec.name.as_str();
        let drbd = Drbd { runner: self.runner };
        match action {
            Action::CreateFile { bytes } => backing::create_sparse_file(&self.paths.image(volume), *bytes)?,
            Action::AttachLoop => *loop_device = Some(backing::attach_loop(self.runner, &self.paths.image(volume))?),
            Action::LinkDevice => {
                // AttachLoop always precedes LinkDevice when no device was observed.
                let device = loop_device.as_deref().unwrap_or_default();
                backing::link_device(&self.paths.device_link(volume), device)?;
            }
            Action::WriteRes => {
                let text = resource::render(spec, &self.paths.device_link(volume));
                write_atomically(&self.paths.res_file(volume), &text)?;
            }
            Action::CreateMd => drbd.create_md(volume, spec.max_peers)?,
            Action::MarkReady => write_atomically(&self.paths.ready_marker(volume), "")?,
            Action::Up => drbd.up(volume)?,
            Action::Adjust => {
                drbd.adjust(volume)?;
            }
            Action::SkipInitialSync => drbd.skip_initial_sync(volume)?,
        }
        Ok(())
    }

    /// Takes the volume down on this node and deletes its local files.
    /// Returns the steps that were performed.
    pub fn remove(&self, volume: &str) -> Result<Vec<&'static str>, ReconcileError> {
        let mut steps = Vec::new();
        let drbd = Drbd { runner: self.runner };
        if drbd.status(volume)?.is_some() {
            drbd.down(volume)?;
            steps.push("down");
        }
        let image = self.paths.image(volume);
        if image.exists()
            && let Some(device) = backing::find_loop(self.runner, &image)?
        {
            backing::detach_loop(self.runner, &device)?;
            steps.push("detach-loop");
        }
        let files = [
            (self.paths.device_link(volume), "remove-link"),
            (self.paths.res_file(volume), "remove-res"),
            (self.paths.ready_marker(volume), "remove-marker"),
            (image, "remove-file"),
        ];
        for (path, step) in files {
            if remove_if_present(&path)? {
                steps.push(step);
            }
        }
        Ok(steps)
    }
}

fn write_atomically(path: &Path, text: &str) -> Result<(), ReconcileError> {
    let io_err = |action, source| ReconcileError::Io {
        action,
        path: path.display().to_string(),
        source,
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| io_err("create directory for", e))?;
    }
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, text).map_err(|e| io_err("write", e))?;
    fs::rename(&tmp, path).map_err(|e| io_err("rename into", e))
}

fn remove_if_present(path: &Path) -> Result<bool, ReconcileError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(ReconcileError::Io {
            action: "remove",
            path: path.display().to_string(),
            source,
        }),
    }
}

#[cfg(test)]
#[path = "tests/reconcile.rs"]
mod tests;
```

- [ ] **Step 4: подкоманды `apply`, `remove`, `status`**

`src/main.rs` целиком:

```rust
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use ambar::drbd::adm::Drbd;
use ambar::exec::SystemRunner;
use ambar::model::VolumeSpec;
use ambar::module_loader::{self, LoaderEnv};
use ambar::paths::Paths;
use ambar::reconcile::Reconciler;

#[derive(Parser)]
#[command(name = "ambar", version, about = "Replicated volumes for k3s on top of DRBD 9")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Load the DRBD 9 kernel module bundled in the image.
    LoadModule {
        #[arg(long, default_value = "/opt/ambar/modules")]
        modules_dir: PathBuf,
    },
    /// Bring this node in line with a volume description.
    Apply {
        /// JSON file with the volume description.
        #[arg(long)]
        file: PathBuf,
        /// Node name; defaults to the host name.
        #[arg(long)]
        node: Option<String>,
    },
    /// Take a volume down on this node and delete its local files.
    Remove {
        #[arg(long)]
        name: String,
    },
    /// Print the DRBD state of a volume on this node.
    Status {
        #[arg(long)]
        name: String,
    },
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("ambar: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<()> {
    let runner = SystemRunner;
    let paths = Paths::default();
    match cli.command {
        Command::LoadModule { modules_dir } => {
            let kernel_release = read_trimmed(Path::new("/proc/sys/kernel/osrelease"))?;
            let env = LoaderEnv {
                modules_dir: &modules_dir,
                sys_module_dir: Path::new("/sys/module"),
                kernel_release: &kernel_release,
            };
            println!("{}", module_loader::load(&env, &runner)?);
        }
        Command::Apply { file, node } => {
            let json = fs::read_to_string(&file).with_context(|| format!("cannot read {}", file.display()))?;
            let spec = VolumeSpec::from_json(&json)?;
            let node = match node {
                Some(node) => node,
                None => read_trimmed(Path::new("/proc/sys/kernel/hostname"))?,
            };
            let done = Reconciler {
                paths: &paths,
                runner: &runner,
            }
            .apply(&spec, &node)?;
            println!("{}: {done:?}", spec.name);
        }
        Command::Remove { name } => {
            let steps = Reconciler {
                paths: &paths,
                runner: &runner,
            }
            .remove(&name)?;
            println!("{name}: {steps:?}");
        }
        Command::Status { name } => match (Drbd { runner: &runner }).status(&name)? {
            None => println!("{name}: not configured"),
            Some(status) => {
                let device = status.device().context("the resource has no volume 0")?;
                println!(
                    "{name}: role={:?} disk={:?} quorum={} open={}",
                    status.role, device.disk_state, device.quorum, device.open
                );
                for conn in &status.connections {
                    let peer_disk = conn.peer_devices.first().map(|p| format!("{:?}", p.peer_disk_state));
                    println!(
                        "  {}: {:?} role={:?} disk={}",
                        conn.name,
                        conn.connection_state,
                        conn.peer_role,
                        peer_disk.as_deref().unwrap_or("-")
                    );
                }
            }
        },
    }
    Ok(())
}

fn read_trimmed(path: &Path) -> Result<String> {
    let text = fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    Ok(text.trim().to_string())
}
```

- [ ] **Step 5: прогнать гейт**

```bash
cargo fmt && cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test 2>&1 | grep -E "^test result" | head -1
```

Ожидается: `test result: ok. 49 passed; 0 failed`.

- [ ] **Step 6: собрать под узлы**

```bash
cargo zigbuild --release --target aarch64-unknown-linux-musl 2>&1 | tail -1 && file target/aarch64-unknown-linux-musl/release/ambar
```

Ожидается: `ELF 64-bit LSB executable, ARM aarch64 … statically linked`. Предупреждение компоновщика `ignoring deprecated linker optimization setting` на сборку не влияет.

- [ ] **Step 7: коммит**

```bash
git add src
git commit -m "feat(agent): reconcile one volume on one node; apply, remove and status commands" -- src
```

---

### Task 8: бинарь в образе и проверка на стенде

**Files:**
- Create: `image/modprobe-blacklist.conf`, `stand/agent-smoke.sh`
- Modify: `image/Dockerfile`, `image/build.sh`, `image/test.sh`, `.gitignore`, `stand/README.md`, `README.md`, `README.ru.md`

**Interfaces:**
- Consumes: бинарь `ambar` (задача 7), стенд и хелперы `stand/lib.sh`, результаты задачи 1.
- Produces: образ `ambar-drbd:dev` с `/usr/local/bin/ambar` и `/etc/modprobe.d/ambar-blacklist-drbd.conf`; скрипт `stand/agent-smoke.sh [--reboot]`, который завершается строкой `OK: agent smoke`.

- [ ] **Step 1: дописать проверку образа**

В `image/test.sh` перед последней строкой `echo "OK: …"` добавить:

```bash
agent="$(run "ambar --version")"
[[ "$agent" == "ambar "* ]] || fail "ambar binary: $agent"
run "grep -q '^install drbd /bin/false' /etc/modprobe.d/ambar-blacklist-drbd.conf" \
  || fail "the in-tree drbd module is not blacklisted in the image"
```

```bash
image/test.sh; echo "rc=$?"
```

Ожидается `FAIL: ambar binary: …` и `rc=1`: в текущем образе бинаря нет.

- [ ] **Step 2: чёрный список для образа**

`image/modprobe-blacklist.conf`:

```text
# The in-tree drbd module is version 8.4 and cannot read DRBD 9 metadata.
# The host has the same file; modprobe inside the container reads this one.
blacklist drbd
install drbd /bin/false
```

- [ ] **Step 3: бинарь в образе**

В `image/Dockerfile` в последней стадии после строки `COPY --chmod=0755 load-module.sh /opt/ambar/load-module.sh` добавить:

```dockerfile
COPY modprobe-blacklist.conf /etc/modprobe.d/ambar-blacklist-drbd.conf
COPY --chmod=0755 ambar /usr/local/bin/ambar
```

В `image/build.sh` перед `docker buildx build` добавить сборку бинаря и копирование в контекст:

```bash
# The agent is cross-compiled on the host and copied into the build context.
(cd .. && cargo zigbuild --release --target aarch64-unknown-linux-musl)
cp ../target/aarch64-unknown-linux-musl/release/ambar ambar
```

В `.gitignore` добавить строку `/image/ambar`.

- [ ] **Step 4: собрать и проверить образ**

```bash
image/build.sh && image/test.sh && image/test-loader.sh | tail -1
```

Ожидается: `OK: ambar-drbd:dev module 9.3.4 for 6.12.116+deb13-arm64, drbd-utils 9.35.0` и `OK: 6 loader cases`.

- [ ] **Step 5: написать сквозную проверку**

`stand/agent-smoke.sh`:

```bash
#!/usr/bin/env bash
# End-to-end check of the agent CLI on the stand: one volume description,
# three nodes (two copies and a node without a disk).
# usage: agent-smoke.sh [--reboot]
#   --reboot  also force-stops and restarts dv-2 and lets the agent restore it
set -euo pipefail
source "$(dirname "$0")/lib.sh"

name=smoke
minor=1010
spec=/var/lib/rancher/ambar/volumes/$name.json
reboot="${1:-}"

fail() { echo "FAIL: $1" >&2; exit 1; }

json="$(cat <<EOF
{
  "name": "$name",
  "sizeBytes": 1073741824,
  "protocol": "C",
  "minor": $minor,
  "port": 7010,
  "maxPeers": 7,
  "nodes": [
    {"name": "$(vm_host dv-1)", "address": "$(vm_ip dv-1)", "nodeId": 0, "diskful": true},
    {"name": "$(vm_host dv-2)", "address": "$(vm_ip dv-2)", "nodeId": 1, "diskful": true},
    {"name": "$(vm_host dv-3)", "address": "$(vm_ip dv-3)", "nodeId": 2, "diskful": false}
  ]
}
EOF
)"

apply() { tb "$1" "ambar apply --file $spec"; }
state() { tb "$1" "ambar status --name $name"; }

# wait_for <node> <pattern> <seconds>: poll `ambar status` until the pattern
# is found.
wait_for() {
    local node="$1" pattern="$2" deadline=$(( $(date +%s) + $3 )) out
    while :; do
        out="$(state "$node")"
        if grep -q -E "$pattern" <<<"$out"; then return 0; fi
        if (( $(date +%s) > deadline )); then
            echo "$out" >&2
            fail "$node: no '$pattern' within $3 s"
        fi
        sleep 2
    done
}

echo "== apply on three nodes"
for n in "${NODES[@]}"; do
    printf '%s\n' "$json" | vm_put "$n" "$spec"
    apply "$n"
done

echo "== the leader skips the initial sync once everyone is connected"
skipped=no
for _ in 1 2 3 4 5 6 7 8 9 10; do
    if apply dv-1 | grep -q SkipInitialSync; then skipped=yes; break; fi
    sleep 2
done
[[ "$skipped" == yes ]] || fail "dv-1 never skipped the initial sync"
wait_for dv-1 "disk=UpToDate" 30
wait_for dv-2 "disk=UpToDate" 30
wait_for dv-3 "disk=Diskless quorum=true" 30
[[ "$(state dv-1 | grep -c ': Connected ')" == 2 ]] || fail "dv-1 does not see two connected peers"

echo "== a settled volume needs nothing"
for n in "${NODES[@]}"; do
    [[ "$(apply "$n")" == "$name: []" ]] || fail "$n: apply is not idempotent"
done

echo "== write through the node without a disk, read on a node with a copy"
tb dv-1 "mkfs.ext4 -q /dev/drbd$minor"
vm dv-3 "mkdir -p /mnt/$name && mount /dev/drbd$minor /mnt/$name && echo ambar > /mnt/$name/f && sync /mnt/$name/f && umount /mnt/$name"
got="$(vm dv-1 "mkdir -p /mnt/$name && mount /dev/drbd$minor /mnt/$name && cat /mnt/$name/f && umount /mnt/$name")"
[[ "$got" == ambar ]] || fail "read back '$got'"

echo "== a copy that lost its loop device and kernel state is restored"
tb dv-2 "drbdadm down $name"
vm dv-2 "losetup -d \$(losetup -j /var/lib/rancher/ambar/volumes/$name.img | cut -d: -f1); rm -f /dev/ambar/$name"
restored="$(apply dv-2)"
[[ "$restored" == "$name: [AttachLoop, LinkDevice, Up]" ]] || fail "dv-2 restore did: $restored"
wait_for dv-2 "disk=UpToDate" 60

if [[ "$reboot" == --reboot ]]; then
    echo "== dv-2 is force-stopped and started again"
    limactl stop --force dv-2
    "$(dirname "$0")/up.sh"
    kc wait --for=condition=Ready node --all --timeout=300s
    kc -n ambar-spike rollout status ds/toolbox --timeout=180s
    tb dv-2 "ambar load-module"
    restored="$(apply dv-2)"
    [[ "$restored" == "$name: [AttachLoop, LinkDevice, Up]" ]] || fail "dv-2 after reboot did: $restored"
    wait_for dv-2 "disk=UpToDate" 120
fi

echo "== remove everywhere"
for n in "${NODES[@]}"; do
    tb "$n" "ambar remove --name $name"
    vm "$n" "rm -f $spec; rmdir /mnt/$name 2>/dev/null || true
        test -z \"\$(ls -A /var/lib/rancher/ambar/volumes)\" || { ls -A /var/lib/rancher/ambar/volumes; exit 1; }
        test ! -e /var/lib/rancher/ambar/res/$name.res
        ! losetup -a | grep -q ambar"
    [[ "$(state "$n")" == "$name: not configured" ]] || fail "$n: resource is still configured"
done

echo "OK: agent smoke"
```

Описание тома кладётся в `/var/lib/rancher/ambar/volumes/smoke.json`: этот каталог узла смонтирован в под `toolbox`.

- [ ] **Step 6: обновить образ на стенде и запустить проверку**

```bash
chmod +x stand/agent-smoke.sh && stand/k3s.sh && stand/agent-smoke.sh
```

`stand/k3s.sh` импортирует пересобранный образ и перезапускает `toolbox`. Ожидается последняя строка `OK: agent smoke`.

Скрипт на стенде не запускался. Если шаг падает из-за самого скрипта, исправьте скрипт. Если падает агент, найдите причину: сравните вывод `ambar status` с образцами, посмотрите `dmesg | grep drbd` на узле. Не меняйте ожидаемые значения в скрипте, чтобы он прошёл.

- [ ] **Step 7: проверка с перезагрузкой узла**

```bash
stand/agent-smoke.sh --reboot
```

Ожидается `OK: agent smoke`. В этом прогоне `dv-2` останавливается принудительно, после старта модуль загружает `ambar load-module`, а `ambar apply` восстанавливает loop-устройство и поднимает ресурс без `create-md`.

- [ ] **Step 8: убедиться, что стенд чист**

```bash
stand/spike/clean.sh && stand/check.sh
```

Ожидается `OK: stand is up`, ресурсов DRBD и loop-устройств нет.

- [ ] **Step 9: документация**

В `stand/README.md` добавить раздел «Проверка агента» с двумя командами из шагов 6 и 7 и ожидаемой строкой.

В `README.md` и `README.ru.md` в таблице состояния заменить строку про драйвер:

- `README.md`: `| CSI driver (Rust agent) | core without Kubernetes: volume model, resource file, status parsing, module loader, local reconcile CLI; checked on the stand |`
- `README.ru.md`: `| CSI-драйвер (агент на Rust) | ядро без Kubernetes: модель тома, файл ресурса, разбор статуса, загрузчик модуля, локальное согласование через CLI; проверено на стенде |`

В обоих файлах в разделе о сборке добавить команду `cargo test` с ожидаемой строкой `test result: ok. 49 passed`.

- [ ] **Step 10: коммит**

```bash
git add image stand README.md README.ru.md .gitignore
git commit -m "feat(image): ship the agent binary; add the stand smoke test" -- image stand README.md README.ru.md .gitignore
```

---

## Проверка плана целиком

После задачи 8:

```bash
cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test 2>&1 | grep -E "^test result" | head -1 && image/test.sh && image/test-loader.sh | tail -1 && stand/check.sh | tail -1
```

Ожидается четыре строки: `test result: ok. 49 passed; 0 failed …`, `OK: ambar-drbd:dev …`, `OK: 6 loader cases`, `OK: stand is up`.

## Откат

Фаза не затрагивает узлы кластера. Стенд очищается `stand/spike/clean.sh`; предыдущий образ собирается из предыдущего коммита командой `image/build.sh` и доставляется на стенд `stand/k3s.sh`.
