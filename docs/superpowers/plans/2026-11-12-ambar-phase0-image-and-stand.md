# ambar, фаза 0: образ с DRBD 9 и стенд — план реализации

> Статус документа: план — исторический журнал. Актуальный код лежит в
> репозитории; отклонения проверок плана от выполненных описаны в
> `docs/superpowers/specs/2026-11-12-drbd9-spike-results.md`, раздел
> «Отклонения от брифа».

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Цель:** собрать образ с модулем DRBD 9 и утилитами, поднять стенд из трёх ВМ с ядром узлов и закрыть шесть допущений из раздела «Что проверяется первым» спеки.

**Архитектура:** образ `ambar-drbd` собирается многостадийным Dockerfile: модуль под заданную версию ядра, `drbd-utils` из исходников, итоговый слой на `debian:trixie-slim`. Стенд — три ВМ Lima на маке с тем же пакетом ядра Debian, что на узлах, и k3s из трёх server. Проверки выполняются из привилегированного пода `toolbox` с этим образом, поэтому тот же образ позже становится базой агента.

**Стек:** Docker buildx, Lima (`vmType: vz`), Debian 13 trixie arm64, k3s, DRBD 9.3.4, drbd-utils 9.35.0, bash.

**Спека:** `docs/superpowers/specs/2026-11-12-ambar-design.md`.

**Что этот план не покрывает:** код агента на Rust (CRD, согласование, CSI), манифесты деплоя, сценарии отказов через Kubernetes, замер на R5C. Для них пишется отдельный план после этого: формат вывода `drbdsetup status --json`, выбор `on-no-quorum` и последовательность пропуска начальной синхронизации берутся из результатов задачи 4.

## Global Constraints

- Версия ядра: `6.12.116+deb13-arm64`. Во всех скриптах — переменная `KVER` с этим значением по умолчанию.
- DRBD: тег `drbd-9.3.4` (`https://github.com/LINBIT/drbd`). drbd-utils: тег `v9.35.0` (`https://github.com/LINBIT/drbd-utils`).
- Сборка модуля — с `SPAAS=false`.
- Модули в образе лежат в `/opt/ambar/modules/<версия ядра>/`.
- Модуль загружается с параметром `usermode_helper=disabled`.
- Данные томов на узле: `/var/lib/rancher/ambar/volumes/<имя>.img`, loop с `--direct-io=on`.
- Образ собирается с `--provenance=false --sbom=false`.
- Комментарии в коде и сообщения коммитов — на английском. Документы в `docs/superpowers/**` — на русском.
- `git commit` выполняется только по явной команде владельца; шаги «Коммит» в задачах ждут её. Трейлер `Co-Authored-By` и подписи об авторстве не добавлять. Работать в `main`, веток не создавать.
- Все команды выполняются на маке из корня репозитория `~/IdeaProjects/ambar`, если в шаге не сказано иное. Shebang скриптов — `#!/usr/bin/env bash`.

## Файлы

| Файл | Назначение |
|---|---|
| `.gitignore` | исключить артефакты |
| `image/Dockerfile` | три стадии: модуль, утилиты, итоговый образ |
| `image/drbd.conf` | `/etc/drbd.conf` образа: без `usage-count`, подключает `*.res` |
| `image/load-module.sh` | загрузка модуля по правилам спеки; прототип загрузчика агента |
| `image/build.sh` | сборка образа `ambar-drbd:dev` |
| `image/test.sh` | проверка образа без загрузки модуля |
| `stand/lib.sh` | общие функции: `vm`, `vm_put`, `vm_ip`, `vm_host`, `kc`, `tb` |
| `stand/lima/node.yaml` | шаблон ВМ |
| `stand/up.sh` | создать три ВМ, поставить ядро `KVER`, перезагрузить |
| `stand/check.sh` | проверить ядро и связность ВМ |
| `stand/down.sh` | удалить ВМ |
| `stand/k3s.sh` | k3s из трёх server, blacklist, импорт образа, `toolbox` |
| `stand/toolbox.yaml` | DaemonSet `toolbox` |
| `stand/spike/res.sh` | создать файлы томов, loop и ресурс DRBD на трёх узлах |
| `stand/spike/probe.sh` | писатель-проба с журналом подтверждённых `fsync` |
| `docs/superpowers/specs/2026-11-12-drbd9-spike-results.md` | результаты шести проверок |

---

### Task 1: образ `ambar-drbd`

**Files:**
- Create: `.gitignore`, `image/Dockerfile`, `image/drbd.conf`, `image/load-module.sh`, `image/build.sh`, `image/test.sh`

**Interfaces:**
- Consumes: ничего.
- Produces: локальный образ `ambar-drbd:dev` (linux/arm64). В нём: `/opt/ambar/modules/$KVER/{drbd.ko,drbd_transport_tcp.ko}`, `/usr/sbin/{drbdadm,drbdsetup,drbdmeta}`, `/etc/drbd.conf`, `/opt/ambar/load-module.sh`, утилиты `losetup`, `mkfs.ext4`, `modprobe`, `insmod`. Скрипт `image/build.sh` принимает `KVER` из окружения.

- [ ] **Step 1: написать проверку образа**

`image/test.sh`:

```bash
#!/usr/bin/env bash
# Verifies the ambar-drbd image without loading the kernel module.
set -euo pipefail

KVER="${KVER:-6.12.116+deb13-arm64}"
IMAGE="${IMAGE:-ambar-drbd:dev}"

run() { docker run --rm --platform linux/arm64 "$IMAGE" bash -c "$1"; }

fail() { echo "FAIL: $1" >&2; exit 1; }

vermagic="$(run "modinfo -F vermagic /opt/ambar/modules/$KVER/drbd.ko")"
[[ "$vermagic" == "$KVER SMP "* ]] || fail "drbd.ko vermagic: $vermagic"

modver="$(run "modinfo -F version /opt/ambar/modules/$KVER/drbd.ko")"
[[ "$modver" == 9.3.4 ]] || fail "drbd.ko version: $modver"

run "test -f /opt/ambar/modules/$KVER/drbd_transport_tcp.ko" \
  || fail "drbd_transport_tcp.ko is missing"

admver="$(run "drbdadm --version | sed -n 's/^DRBDADM_VERSION=//p'")"
[[ "$admver" == 9.35.0 ]] || fail "drbdadm version: $admver"

run "grep -q 'usage-count no' /etc/drbd.conf" || fail "usage-count is not disabled"
run "command -v losetup mkfs.ext4 modprobe insmod >/dev/null" || fail "host tools are missing"
run "test -x /opt/ambar/load-module.sh" || fail "load-module.sh is missing"

echo "OK: $IMAGE module $modver for $KVER, drbd-utils $admver"
```

- [ ] **Step 2: убедиться, что проверка падает**

```bash
chmod +x image/test.sh && image/test.sh
```

Ожидается: ненулевой код возврата, в выводе `Unable to find image 'ambar-drbd:dev' locally`.

- [ ] **Step 3: написать `image/drbd.conf`**

```
# ambar: resources are generated per volume into /etc/drbd.d
global {
    usage-count no;
}
include "/etc/drbd.d/*.res";
```

- [ ] **Step 4: написать `image/load-module.sh`**

```bash
#!/usr/bin/env bash
# Loads the bundled DRBD 9 module for the running kernel.
# Refuses to continue when the in-tree DRBD 8.4 module is already loaded.
set -euo pipefail

MODULES_DIR="${MODULES_DIR:-/opt/ambar/modules}"

if [[ -d /sys/module/drbd ]]; then
    loaded="$(cat /sys/module/drbd/version)"
    case "$loaded" in
        9.*) echo "drbd $loaded is already loaded"; exit 0 ;;
        *)   echo "in-tree drbd 8.4 is loaded (version $loaded)" >&2; exit 1 ;;
    esac
fi

kver="$(uname -r)"
dir="$MODULES_DIR/$kver"
if [[ ! -f "$dir/drbd.ko" ]]; then
    echo "no drbd module for kernel $kver" >&2
    exit 1
fi

# Dependencies come from the host module tree mounted read-only at /lib/modules.
modprobe -a lru_cache libcrc32c
insmod "$dir/drbd.ko" usermode_helper=disabled
insmod "$dir/drbd_transport_tcp.ko"
echo "loaded drbd $(cat /sys/module/drbd/version) for kernel $kver"
```

- [ ] **Step 5: написать `image/Dockerfile`**

```dockerfile
# syntax=docker/dockerfile:1

ARG KVER=6.12.116+deb13-arm64
ARG DRBD_TAG=drbd-9.3.4
ARG UTILS_TAG=v9.35.0

# --- kernel module -----------------------------------------------------------
FROM debian:trixie AS module
ARG KVER
ARG DRBD_TAG
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      git ca-certificates build-essential kmod coccinelle python3 \
      "linux-headers-${KVER}" \
 && rm -rf /var/lib/apt/lists/*
RUN git clone --depth 1 --branch "${DRBD_TAG}" --recursive --shallow-submodules \
      https://github.com/LINBIT/drbd /src
# SPAAS=false: generate the compat patch with local coccinelle, no network calls.
RUN make -j"$(nproc)" -C /src/drbd KDIR="/lib/modules/${KVER}/build" SPAAS=false
RUN mkdir -p "/out/${KVER}" \
 && cp "/src/drbd/build-${KVER}/drbd.ko" \
       "/src/drbd/build-${KVER}/drbd_transport_tcp.ko" "/out/${KVER}/"

# --- userland tools ----------------------------------------------------------
FROM debian:trixie AS utils
ARG UTILS_TAG
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      git ca-certificates build-essential autoconf automake flex \
      libkeyutils-dev pkg-config \
 && rm -rf /var/lib/apt/lists/*
RUN git clone --depth 1 --branch "${UTILS_TAG}" --recursive --shallow-submodules \
      https://github.com/LINBIT/drbd-utils /src
WORKDIR /src
RUN ./autogen.sh \
 && ./configure --prefix=/usr --localstatedir=/var --sysconfdir=/etc \
      --without-84support --without-manual --without-heartbeat \
      --without-pacemaker --without-rgmanager --without-bashcompletion \
      --with-udev=no --with-initscripttype=none \
 && make -j"$(nproc)" \
 && make install DESTDIR=/stage

# --- runtime -----------------------------------------------------------------
FROM debian:trixie-slim
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      kmod util-linux e2fsprogs libkeyutils1 \
 && rm -rf /var/lib/apt/lists/*
COPY --from=module /out/ /opt/ambar/modules/
COPY --from=utils /stage/usr/sbin/drbdadm /stage/usr/sbin/drbdsetup \
     /stage/usr/sbin/drbdmeta /usr/sbin/
COPY drbd.conf /etc/drbd.conf
COPY --chmod=0755 load-module.sh /opt/ambar/load-module.sh
RUN mkdir -p /etc/drbd.d /var/lib/drbd /var/run/drbd
```

- [ ] **Step 6: написать `image/build.sh` и `.gitignore`**

`image/build.sh`:

```bash
#!/usr/bin/env bash
# Builds the ambar-drbd image for one kernel version.
set -euo pipefail

KVER="${KVER:-6.12.116+deb13-arm64}"
IMAGE="${IMAGE:-ambar-drbd:dev}"
cd "$(dirname "$0")"

docker buildx build \
  --platform linux/arm64 \
  --provenance=false --sbom=false \
  --build-arg "KVER=${KVER}" \
  --tag "$IMAGE" \
  --load .
```

`.gitignore`:

```
/target/
/stand/.state/
*.tar
```

- [ ] **Step 7: собрать образ и прогнать проверку**

```bash
chmod +x image/build.sh image/load-module.sh && image/build.sh && image/test.sh
```

Ожидается последняя строка:
`OK: ambar-drbd:dev module 9.3.4 for 6.12.116+deb13-arm64, drbd-utils 9.35.0`

Если `apt-get` не находит `linux-headers-6.12.116+deb13-arm64`, пакет вытеснен из `trixie-security` более новой версией. Тогда в стадию `module` перед `apt-get update` добавляется источник `snapshot.debian.org` на дату, когда пакет был в архиве; `KVER` не менять, он должен совпадать с ядром узлов.

- [ ] **Step 8: коммит (по команде владельца)**

```bash
git add .gitignore image/ docs/
git commit -m "feat(image): build DRBD 9.3.4 module and drbd-utils 9.35.0 image"
```

---

### Task 2: стенд из трёх ВМ с ядром узлов

**Files:**
- Create: `stand/lib.sh`, `stand/lima/node.yaml`, `stand/up.sh`, `stand/check.sh`, `stand/down.sh`

**Interfaces:**
- Consumes: ничего.
- Produces: три ВМ Lima `dv-1`, `dv-2`, `dv-3` с ядром `$KVER`. Функции в `stand/lib.sh`:
  - `vm <узел> <команда...>` — выполнить команду от root в ВМ;
  - `vm_put <узел> <путь в ВМ>` — записать stdin в файл в ВМ от root;
  - `vm_ip <узел>` — адрес ВМ в общей сети;
  - `vm_host <узел>` — `uname -n` ВМ;
  - массив `NODES=(dv-1 dv-2 dv-3)`, переменные `KVER`, `NET_IF`.

- [ ] **Step 1: установить Lima**

Требуется согласие владельца на установку.

```bash
brew install lima
```

```bash
limactl --version
```

Ожидается: `limactl version 1.` или новее. Версия записывается в результаты задачи 4.

- [ ] **Step 2: написать `stand/lib.sh`**

```bash
#!/usr/bin/env bash
# Shared helpers for the ambar test stand. Source this file.
set -euo pipefail

NODES=(dv-1 dv-2 dv-3)
KVER="${KVER:-6.12.116+deb13-arm64}"
# Interface of the network shared between the VMs.
NET_IF="${NET_IF:-eth0}"

# vm <node> <command...>: run a command as root inside the VM.
vm() {
    local node="$1"; shift
    limactl shell --workdir / "$node" sudo bash -c "$*"
}

# vm_put <node> <path>: write stdin to a root-owned file inside the VM.
vm_put() {
    limactl shell --workdir / "$1" sudo tee "$2" >/dev/null
}

# vm_ip <node>: IPv4 address on the shared network.
vm_ip() {
    limactl shell --workdir / "$1" ip -4 -o addr show dev "$NET_IF" \
        | awk '{print $4}' | cut -d/ -f1
}

# vm_host <node>: hostname as DRBD sees it.
vm_host() {
    limactl shell --workdir / "$1" uname -n
}
```

- [ ] **Step 3: написать `stand/lima/node.yaml`**

```yaml
# Debian 13 "generic" image: ships cloud-init and the linux-image-arm64 flavour,
# the same kernel flavour as on the k3s nodes ("genericcloud" uses a different one).
vmType: vz
arch: aarch64
images:
  - location: "https://cloud.debian.org/images/cloud/trixie/latest/debian-13-generic-arm64.qcow2"
    arch: aarch64
cpus: 2
memory: "3GiB"
disk: "20GiB"
mounts: []
containerd:
  system: false
  user: false
networks:
  - lima: user-v2
```

- [ ] **Step 4: написать проверку стенда `stand/check.sh`**

```bash
#!/usr/bin/env bash
# Verifies kernel version and VM-to-VM connectivity.
set -euo pipefail
source "$(dirname "$0")/lib.sh"

for n in "${NODES[@]}"; do
    k="$(limactl shell --workdir / "$n" uname -r)"
    [[ "$k" == "$KVER" ]] || { echo "FAIL: $n runs $k, want $KVER" >&2; exit 1; }
    echo "$n: kernel $k, host $(vm_host "$n"), ip $(vm_ip "$n")"
done

for a in "${NODES[@]}"; do
    for b in "${NODES[@]}"; do
        [[ "$a" == "$b" ]] && continue
        vm "$a" "ping -c1 -W2 $(vm_ip "$b") >/dev/null" \
            || { echo "FAIL: $a cannot reach $b" >&2; exit 1; }
    done
done
echo "OK: stand is up"
```

- [ ] **Step 5: убедиться, что проверка падает**

```bash
chmod +x stand/check.sh && stand/check.sh
```

Ожидается: ненулевой код возврата, Lima сообщает, что экземпляра `dv-1` нет.

- [ ] **Step 6: написать `stand/up.sh` и `stand/down.sh`**

`stand/up.sh`:

```bash
#!/usr/bin/env bash
# Creates three VMs and boots them into the kernel version used on the k3s nodes.
set -euo pipefail
source "$(dirname "$0")/lib.sh"
TEMPLATE="$(dirname "$0")/lima/node.yaml"

for n in "${NODES[@]}"; do
    if ! limactl list -q | grep -qx "$n"; then
        limactl start --tty=false --name "$n" "$TEMPLATE"
    fi
done

for n in "${NODES[@]}"; do
    if [[ "$(limactl shell --workdir / "$n" uname -r)" == "$KVER" ]]; then
        continue
    fi
    vm "$n" "export DEBIAN_FRONTEND=noninteractive
        apt-get update -qq
        apt-get install -y -qq linux-image-$KVER nftables
        mkdir -p /etc/default/grub.d
        cat > /etc/default/grub.d/99-ambar.cfg <<EOF
GRUB_DEFAULT='Advanced options for Debian GNU/Linux>Debian GNU/Linux, with Linux $KVER'
EOF
        update-grub"
    limactl stop "$n"
    limactl start --tty=false "$n"
done

"$(dirname "$0")/check.sh"
```

`stand/down.sh`:

```bash
#!/usr/bin/env bash
# Deletes the stand VMs and their disks.
set -euo pipefail
source "$(dirname "$0")/lib.sh"

for n in "${NODES[@]}"; do
    limactl delete --force "$n" || true
done
```

- [ ] **Step 7: поднять стенд**

```bash
chmod +x stand/up.sh stand/down.sh && stand/up.sh
```

Ожидается: три строки вида `dv-1: kernel 6.12.116+deb13-arm64, host lima-dv-1, ip 192.168.104.N` и `OK: stand is up`.

Возможные расхождения и что с ними делать:

| Симптом | Причина | Действие |
|---|---|---|
| `FAIL: dv-N runs <другое ядро>` | GRUB загрузил другое ядро: имя пункта меню не совпало | В ВМ выполнить `grep menuentry /boot/grub/grub.cfg`, подставить точное имя пункта в `GRUB_DEFAULT` в `stand/up.sh` |
| `ip: Device "eth0" does not exist` | интерфейс общей сети назван иначе | `limactl shell dv-1 ip -4 -o addr`, запускать с `NET_IF=<имя>` и исправить значение по умолчанию в `stand/lib.sh` |
| `FAIL: dv-A cannot reach dv-B` | сеть `user-v2` не связывает ВМ в этой версии Lima | Сверить раздел `networks` с документацией установленной версии Lima (`limactl start --help`, шаблоны в `$(brew --prefix)/share/lima/templates`) |

Фактические значения (имя интерфейса, имена хостов, версия Lima) записываются в результаты задачи 4.

- [ ] **Step 8: коммит (по команде владельца)**

```bash
git add stand/
git commit -m "feat(stand): three Lima VMs on the node kernel"
```

---

### Task 3: k3s на стенде и под `toolbox`

**Files:**
- Create: `stand/k3s.sh`, `stand/toolbox.yaml`
- Modify: `stand/lib.sh` (добавить `kc` и `tb`)

**Interfaces:**
- Consumes: `ambar-drbd:dev` (задача 1); `vm`, `vm_put`, `vm_ip`, `vm_host`, `NODES` (задача 2).
- Produces: кластер k3s из трёх server на стенде; DaemonSet `toolbox` в namespace `ambar-spike`; на каждом узле загружен модуль DRBD 9. Функции:
  - `kc <аргументы kubectl...>` — `kubectl` на `dv-1`;
  - `tb <узел> <команда...>` — выполнить команду в поде `toolbox` этого узла.
  Каталоги узла: `/var/lib/rancher/ambar/volumes` (файлы томов), `/var/lib/rancher/ambar/res` (смонтирован в под как `/etc/drbd.d`).

- [ ] **Step 1: добавить `kc` и `tb` в конец `stand/lib.sh`**

```bash
# kc <kubectl args...>: kubectl against the stand cluster.
kc() {
    limactl shell --workdir / dv-1 sudo k3s kubectl "$@"
}

# tb <node> <command...>: run a command in the toolbox pod of that node.
tb() {
    local node="$1"; shift
    local host pod
    host="$(vm_host "$node")"
    pod="$(kc -n ambar-spike get pod -l app=toolbox \
        --field-selector "spec.nodeName=$host" -o name)"
    kc -n ambar-spike exec "$pod" -- bash -c "$*"
}
```

- [ ] **Step 2: написать `stand/toolbox.yaml`**

```yaml
apiVersion: v1
kind: Namespace
metadata:
  name: ambar-spike
---
apiVersion: apps/v1
kind: DaemonSet
metadata:
  name: toolbox
  namespace: ambar-spike
spec:
  selector:
    matchLabels: {app: toolbox}
  template:
    metadata:
      labels: {app: toolbox}
    spec:
      hostNetwork: true
      tolerations:
        - operator: Exists
      containers:
        - name: toolbox
          image: ambar-drbd:dev
          imagePullPolicy: Never
          command: ["sleep", "infinity"]
          securityContext:
            privileged: true
          volumeMounts:
            - {name: dev, mountPath: /dev}
            - {name: modules, mountPath: /lib/modules, readOnly: true}
            - {name: res, mountPath: /etc/drbd.d}
            - {name: volumes, mountPath: /var/lib/rancher/ambar/volumes}
      volumes:
        - name: dev
          hostPath: {path: /dev}
        - name: modules
          hostPath: {path: /lib/modules}
        - name: res
          hostPath: {path: /var/lib/rancher/ambar/res, type: DirectoryOrCreate}
        - name: volumes
          hostPath: {path: /var/lib/rancher/ambar/volumes, type: DirectoryOrCreate}
```

- [ ] **Step 3: написать `stand/k3s.sh`**

```bash
#!/usr/bin/env bash
# Installs a three-server k3s cluster on the stand, blacklists the in-tree drbd
# module, imports the ambar-drbd image and starts the toolbox DaemonSet.
set -euo pipefail
source "$(dirname "$0")/lib.sh"
IMAGE="${IMAGE:-ambar-drbd:dev}"
HERE="$(dirname "$0")"

first_ip="$(vm_ip dv-1)"

for n in "${NODES[@]}"; do
    printf 'blacklist drbd\ninstall drbd /bin/false\n' \
        | vm_put "$n" /etc/modprobe.d/ambar-blacklist-drbd.conf
    vm "$n" "apt-get install -y -qq curl >/dev/null"
done

if ! vm dv-1 "test -x /usr/local/bin/k3s"; then
    vm dv-1 "curl -sfL https://get.k3s.io | sh -s - server --cluster-init \
        --node-ip $first_ip --disable=traefik --disable=servicelb"
fi
token="$(vm dv-1 "cat /var/lib/rancher/k3s/server/node-token")"

for n in dv-2 dv-3; do
    if ! vm "$n" "test -x /usr/local/bin/k3s"; then
        vm "$n" "curl -sfL https://get.k3s.io | K3S_TOKEN='$token' sh -s - server \
            --server https://$first_ip:6443 --node-ip $(vm_ip "$n") \
            --disable=traefik --disable=servicelb"
    fi
done

kc wait --for=condition=Ready node --all --timeout=180s

for n in "${NODES[@]}"; do
    docker save "$IMAGE" | limactl shell --workdir / "$n" sudo k3s ctr images import -
done

kc apply -f - < "$HERE/toolbox.yaml"
kc -n ambar-spike rollout status ds/toolbox --timeout=120s

for n in "${NODES[@]}"; do
    tb "$n" "/opt/ambar/load-module.sh"
done
```

- [ ] **Step 4: запустить и проверить**

```bash
chmod +x stand/k3s.sh && stand/k3s.sh
```

Ожидается в конце три строки `loaded drbd 9.3.4 for kernel 6.12.116+deb13-arm64`.

```bash
source stand/lib.sh && for n in "${NODES[@]}"; do tb "$n" "cat /sys/module/drbd/version; drbdadm --version | grep -E 'DRBD_KERNEL_VERSION=|DRBDADM_VERSION='; cat /sys/module/drbd/parameters/usermode_helper"; done
```

Ожидается на каждом узле: `9.3.4`, `DRBD_KERNEL_VERSION=9.3.4`, `DRBDADM_VERSION=9.35.0`, `disabled`.

Это закрывает допущения 1 (модуль загружается с `usermode_helper=disabled`) и 6 (утилиты работают из контейнера). Если `insmod` завершился ошибкой, полный текст из `dmesg | tail -20` на узле записывается в результаты задачи 4, и план останавливается: остальные задачи без модуля невыполнимы.

- [ ] **Step 5: проверить отказ при загруженном модуле 8.4**

На `dv-3` выгрузить модуль 9, загрузить штатный в обход blacklist и запустить загрузчик:

```bash
source stand/lib.sh
tb dv-3 "rmmod drbd_transport_tcp drbd"
vm dv-3 "modprobe --ignore-install drbd && cat /sys/module/drbd/version"
tb dv-3 "/opt/ambar/load-module.sh"; echo "rc=$?"
```

Ожидается: версия `8.4.`…, затем `in-tree drbd 8.4 is loaded (version 8.4.…)` и `rc=1`.

Вернуть узел в рабочее состояние:

```bash
vm dv-3 "rmmod drbd" && tb dv-3 "/opt/ambar/load-module.sh"
```

Ожидается: `loaded drbd 9.3.4 for kernel 6.12.116+deb13-arm64`.

- [ ] **Step 6: коммит (по команде владельца)**

```bash
git add stand/
git commit -m "feat(stand): k3s cluster with a privileged DRBD toolbox pod"
```

---

### Task 4: проверка допущений DRBD 9

**Files:**
- Create: `stand/spike/res.sh`, `stand/spike/probe.sh`, `docs/superpowers/specs/2026-11-12-drbd9-spike-results.md`

**Interfaces:**
- Consumes: `vm`, `vm_put`, `vm_ip`, `vm_host`, `tb`, `NODES` (задачи 2 и 3).
- Produces:
  - `stand/spike/res.sh <имя> <minor> <порт> <узел без диска|-> <io-error|suspend-io>` — создаёт ресурс на трёх узлах и поднимает его;
  - файл результатов с заполненной таблицей по шести допущениям и образцами вывода `drbdsetup status --json` — вход для плана агента.

- [ ] **Step 1: написать `stand/spike/res.sh`**

```bash
#!/usr/bin/env bash
# Creates backing files, loop devices and a three-node DRBD resource.
# usage: res.sh <name> <minor> <port> <diskless-node|-> <io-error|suspend-io>
set -euo pipefail
source "$(dirname "$0")/../lib.sh"

name="$1"; minor="$2"; port="$3"; diskless="${4:--}"; on_no_quorum="${5:-io-error}"
size="1G"
vol_dir=/var/lib/rancher/ambar/volumes
res_dir=/var/lib/rancher/ambar/res

on_block() {
    local node="$1" id="$2" disk_line
    if [[ "$node" == "$diskless" ]]; then
        disk_line="disk none;"
    else
        disk_line="disk /dev/ambar/$name; meta-disk internal;"
    fi
    cat <<EOF
    on $(vm_host "$node") {
        node-id $id;
        address ipv4 $(vm_ip "$node"):$port;
        volume 0 { device minor $minor; $disk_line }
    }
EOF
}

res="$(cat <<EOF
resource $name {
    options { quorum majority; on-no-quorum $on_no_quorum; auto-promote yes; }
    net { protocol C; }
$(on_block dv-1 0)
$(on_block dv-2 1)
$(on_block dv-3 2)
    connection-mesh { hosts $(vm_host dv-1) $(vm_host dv-2) $(vm_host dv-3); }
}
EOF
)"

for n in "${NODES[@]}"; do
    vm "$n" "mkdir -p $vol_dir $res_dir /dev/ambar"
    printf '%s\n' "$res" | vm_put "$n" "$res_dir/$name.res"
    if [[ "$n" != "$diskless" ]]; then
        vm "$n" "f=$vol_dir/$name.img
            test -f \$f || truncate -s $size \$f
            dev=\$(losetup -j \$f | cut -d: -f1)
            test -n \"\$dev\" || dev=\$(losetup --find --show --direct-io=on \$f)
            ln -sfn \$dev /dev/ambar/$name"
        tb "$n" "drbdadm create-md --force $name"
    fi
    tb "$n" "drbdadm up $name"
done

sleep 5
tb dv-1 "drbdadm status $name"
```

- [ ] **Step 2: написать `stand/spike/probe.sh`**

Скрипт выполняется внутри ВМ. Он дописывает номер в файл на томе, вызывает `fsync` и только после успеха записывает номер в журнал вне тома.

```bash
#!/usr/bin/env bash
# Writer probe: every number in the ack file was fsynced to the volume first.
# usage: probe.sh <mountpoint> <ack-file>
set -uo pipefail

mnt="$1"; ack="$2"
i=0
while :; do
    i=$((i + 1))
    if echo "$i" >> "$mnt/log" && sync "$mnt/log"; then
        echo "$i" > "$ack"
    else
        echo "write $i failed" >&2
        exit 1
    fi
done
```

- [ ] **Step 3: создать файл результатов с пустой таблицей**

`docs/superpowers/specs/2026-11-12-drbd9-spike-results.md`:

```markdown
# Результаты проверки допущений DRBD 9 на стенде

Стенд: три ВМ Lima на маке, Debian 13 trixie arm64, ядро
`6.12.116+deb13-arm64`, DRBD 9.3.4, drbd-utils 9.35.0. Каждое допущение взято
из раздела «Что проверяется первым» спеки
`2026-11-12-ambar-design.md`.

## Окружение

| Что | Значение |
|---|---|
| Версия Lima | |
| Версия k3s | |
| Интерфейс общей сети в ВМ | |
| Имена хостов ВМ | |

## Итог по допущениям

| № | Допущение | Результат | Что меняется в спеке |
|---|---|---|---|
| 1 | Модуль 9.3.4 загружается с `usermode_helper=disabled` | | |
| 2 | Ресурс из трёх сторон поверх loop с `--direct-io=on` поднимается | | |
| 3 | Начальная синхронизация пропускается через `new-current-uuid --clear-bitmap` | | |
| 4 | Сторона без диска участвует в кворуме и открывает устройство на запись | | |
| 5 | `on-no-quorum`: `io-error` или `suspend-io` | | |
| 6 | `drbdadm` и `drbdsetup` работают из контейнера | | |

## Протокол
```

Столбец «Результат» заполняется значениями «подтверждено» или «не подтверждено» с одной фразой о наблюдении. Под заголовком «Протокол» для каждого шага ниже добавляется подраздел с командой и её фактическим выводом.

- [ ] **Step 4: допущения 2 и 3 — ресурс из трёх копий без начальной синхронизации**

```bash
chmod +x stand/spike/res.sh stand/spike/probe.sh
source stand/lib.sh
stand/spike/res.sh spike 1000 7000 - io-error
```

Ожидается в выводе `drbdadm status spike`: роль `Secondary`, локальный диск `Inconsistent`, оба соединения `Connected` с `peer-disk:Inconsistent`.

Проверить прямой ввод-вывод loop-устройства:

```bash
vm dv-1 "losetup -l -O NAME,BACK-FILE,DIO | grep spike"
```

Ожидается: в столбце `DIO` значение `1`.

Пропустить начальную синхронизацию:

```bash
tb dv-1 "drbdadm new-current-uuid --clear-bitmap spike/0" && sleep 3 && tb dv-1 "drbdadm status spike"
```

Ожидается: диск `UpToDate` локально и у обоих пиров, без строки `replication:SyncSource`.

Если команда отвергнута или диски остались `Inconsistent`, выполнить запасной вариант и записать оба результата:

```bash
tb dv-1 "drbdadm primary --force spike && drbdadm secondary spike" && sleep 3 && tb dv-1 "drbdadm status spike"
```

В этом варианте идёт полная синхронизация 1 ГиБ; время до `UpToDate` на всех сторонах записывается в протокол.

Сохранить образец вывода для разбора в агенте:

```bash
tb dv-1 "drbdsetup status spike --json" > /tmp/spike-status.json && cat /tmp/spike-status.json
```

Вывод целиком вставляется в протокол блоком `json`.

- [ ] **Step 5: `auto-promote` и запрет второго открытия**

```bash
tb dv-1 "mkfs.ext4 -q /dev/drbd1000"
vm dv-1 "mkdir -p /mnt/spike && mount /dev/drbd1000 /mnt/spike"
tb dv-1 "drbdadm status spike | head -2"
vm dv-2 "mkdir -p /mnt/spike && mount /dev/drbd1000 /mnt/spike"; echo "rc=$?"
```

Ожидается: после монтирования на `dv-1` роль `Primary`; монтирование на `dv-2` завершается с `rc` не равным 0. Текст ошибки `mount` записывается в протокол: по нему агент будет отличать «том открыт на другом узле» от прочих ошибок.

- [ ] **Step 6: допущение 5 с `io-error` — изоляция узла с писателем**

Запустить писателя на `dv-1`:

```bash
cat stand/spike/probe.sh | vm_put dv-1 /usr/local/bin/probe.sh
vm dv-1 "chmod +x /usr/local/bin/probe.sh; nohup /usr/local/bin/probe.sh /mnt/spike /var/tmp/acked >/var/tmp/probe.log 2>&1 &"
sleep 5 && vm dv-1 "cat /var/tmp/acked"
```

Ожидается: число больше 0.

Изолировать `dv-1` по порту репликации:

```bash
vm dv-1 "nft add table inet spike
  nft add chain inet spike in '{ type filter hook input priority 0; }'
  nft add chain inet spike out '{ type filter hook output priority 0; }'
  nft add rule inet spike in tcp dport 7000 drop
  nft add rule inet spike in tcp sport 7000 drop
  nft add rule inet spike out tcp dport 7000 drop
  nft add rule inet spike out tcp sport 7000 drop"
sleep 20
vm dv-1 "cat /var/tmp/acked; cat /var/tmp/probe.log; pgrep -f probe.sh || echo 'probe exited'"
tb dv-1 "drbdadm status spike"
vm dv-1 "dmesg | grep -i -E 'quorum|drbd spike' | tail -15"
```

Ожидается: `write N failed` в `probe.log`, `probe exited`, в статусе `quorum:no`. Записать в протокол: через сколько секунд после изоляции остановился писатель, строки `dmesg` о кворуме и последнее число в `acked` (далее `ACKED`).

Поднять том на `dv-2` и сверить подтверждённые записи:

```bash
vm dv-2 "mount /dev/drbd1000 /mnt/spike && tail -1 /mnt/spike/log"
```

Ожидается: монтирование успешно, число не меньше `ACKED`. Меньшее число означает потерю подтверждённой записи; это провал проверки, а не наблюдение.

Проверить размонтирование на изолированном узле:

```bash
vm dv-1 "umount /mnt/spike"; echo "rc=$?"
```

Записать `rc` и текст ошибки, если она есть.

Вернуть сеть и наблюдать возврат `dv-1`:

```bash
vm dv-1 "nft delete table inet spike"
sleep 15 && tb dv-2 "drbdadm status spike"
```

Ожидается: `dv-1` в роли `Secondary`, диск `UpToDate` после досылки. Если соединение `dv-1` осталось в `StandAlone`, записать `dmesg | grep -i split` с обоих узлов.

Привести стенд в исходное состояние:

```bash
vm dv-2 "umount /mnt/spike"
```

- [ ] **Step 7: допущение 5 с `suspend-io`**

Сменить политику на всех узлах и применить:

```bash
for n in "${NODES[@]}"; do
  vm "$n" "sed -i 's/on-no-quorum io-error/on-no-quorum suspend-io/' /var/lib/rancher/ambar/res/spike.res"
  tb "$n" "drbdadm adjust spike"
done
tb dv-1 "drbdsetup show spike | grep on-no-quorum"
```

Ожидается: `on-no-quorum suspend-io;`.

Повторить шаг 6 целиком с теми же командами (монтирование на `dv-1`, писатель, изоляция, монтирование на `dv-2`, `umount` на `dv-1`, возврат сети). Перед запуском писателя удалить старый журнал: `vm dv-1 "rm -f /var/tmp/acked /var/tmp/probe.log"`.

Отличия, которые нужно зафиксировать:

| Наблюдение | Как снять |
|---|---|
| Писатель завис или упал | `vm dv-1 "pgrep -f probe.sh && cat /proc/\$(pgrep -f probe.sh | head -1)/status | grep State"` |
| `umount` на изолированном узле | `vm dv-1 "timeout 20 umount /mnt/spike"; echo rc=$?` (`rc=124` означает зависание) |
| Принудительное размонтирование | `vm dv-1 "umount -l /mnt/spike"; echo rc=$?` |
| Состояние после возврата сети | `tb dv-2 "drbdadm status spike"` |

Решение для спеки: если при `suspend-io` `umount` зависает до возврата кворума, остаётся `io-error` (kubelet не сможет освободить том на изолированном узле). Решение и его обоснование записываются в строку 5 таблицы.

После проверки вернуть `io-error` тем же `sed` в обратную сторону и `drbdadm adjust spike`.

- [ ] **Step 8: допущение 4 — сторона без диска**

```bash
stand/spike/res.sh spike2 1001 7001 dv-3 io-error
tb dv-1 "drbdadm new-current-uuid --clear-bitmap spike2/0" && sleep 3 && tb dv-3 "drbdadm status spike2"
```

Ожидается на `dv-3`: локальный диск `Diskless`, у пиров `peer-disk:UpToDate`. Если на шаге 4 сработал только запасной вариант, применить его и здесь.

Открыть том на узле без диска и писать:

```bash
tb dv-1 "mkfs.ext4 -q /dev/drbd1001"
vm dv-3 "mkdir -p /mnt/spike2 && mount /dev/drbd1001 /mnt/spike2"
cat stand/spike/probe.sh | vm_put dv-3 /usr/local/bin/probe.sh
vm dv-3 "chmod +x /usr/local/bin/probe.sh; nohup /usr/local/bin/probe.sh /mnt/spike2 /var/tmp/acked2 >/var/tmp/probe2.log 2>&1 &"
sleep 5 && vm dv-3 "cat /var/tmp/acked2" && tb dv-3 "drbdadm status spike2 | head -2"
```

Ожидается: число больше 0, роль `Primary` при диске `Diskless`.

Выключить один узел с копией и проверить, что запись продолжается. Выключается `dv-2`: через `dv-1` работают `kc` и `tb`.

```bash
limactl stop dv-2
sleep 15
vm dv-3 "a=\$(cat /var/tmp/acked2); sleep 5; b=\$(cat /var/tmp/acked2); echo \"\$a -> \$b\"; pgrep -f probe.sh >/dev/null && echo 'probe alive'"
tb dv-3 "drbdadm status spike2"
```

Ожидается: второе число больше первого, `probe alive`, `quorum:yes` не теряется. Если запись остановилась, узел без диска в этой версии не даёт голос в кворуме при двух копиях; тогда в спеке `replicas: 2` получает ограничение, и оно записывается в строку 4 таблицы.

Вернуть узел:

```bash
limactl start --tty=false dv-2
source stand/lib.sh
kc wait --for=condition=Ready node --all --timeout=180s
kc -n ambar-spike rollout status ds/toolbox --timeout=120s
tb dv-2 "/opt/ambar/load-module.sh"
vm dv-2 "mkdir -p /dev/ambar; for r in spike spike2; do f=/var/lib/rancher/ambar/volumes/\$r.img; ln -sfn \$(losetup --find --show --direct-io=on \$f) /dev/ambar/\$r; done"
tb dv-2 "drbdadm up spike; drbdadm up spike2" && sleep 10 && tb dv-2 "drbdadm status"
```

Ожидается: оба ресурса на `dv-2` `UpToDate`; для `spike2` перед этим кратко видна досылка (`replication:SyncTarget`). В протокол записать, была ли синхронизация полной или по битовой карте: `vm dv-2 "dmesg | grep -i -E 'resync|bitmap' | tail -5"`.

Остановить писателя: `vm dv-3 "pkill -f probe.sh; umount /mnt/spike2"`.

- [ ] **Step 9: замер `fsync` на стенде**

Числа со стенда не переносятся на R5C; замер показывает относительный вклад слоёв.

```bash
vm_put dv-1 /usr/local/bin/fsync-bench.sh <<'EOF'
#!/usr/bin/env bash
# Average latency of 200 single-block writes with fsync, per directory.
set -euo pipefail
for d in "$@"; do
    t0=$(date +%s%N)
    for i in $(seq 200); do
        dd if=/dev/zero of="$d/f" bs=4k count=1 conv=fsync status=none
    done
    echo "$d: $(( ($(date +%s%N) - t0) / 200000 )) us per fsync"
done
EOF
vm dv-1 "chmod +x /usr/local/bin/fsync-bench.sh; mkdir -p /var/tmp/local; mount /dev/drbd1000 /mnt/spike; /usr/local/bin/fsync-bench.sh /var/tmp/local /mnt/spike; umount /mnt/spike"
```

Ожидается две строки вида `<каталог>: N us per fsync`. Обе записываются в протокол с пометкой «ВМ, не R5C».

- [ ] **Step 10: заполнить результаты и сверить спеку**

1. Заполнить таблицы «Окружение» и «Итог по допущениям» в `docs/superpowers/specs/2026-11-12-drbd9-spike-results.md`. В файле не должно остаться пустых ячеек.
2. Для каждой строки с непустым столбцом «Что меняется в спеке» внести правку в `docs/superpowers/specs/2026-11-12-ambar-design.md`: раздел «Параметры ресурса DRBD» (выбор `on-no-quorum`), «Создание» (последовательность пропуска синхронизации), таблица `StorageClass` (ограничение `replicas: 2`, если оно появилось).
3. Удалить из спеки пометки `[проверить на стенде: …]`, закрытые этим планом. Пометка про минимальную версию `drbd-utils` заменяется фактом: «проверено с 9.35.0».

Проверка:

```bash
grep -n "| |" docs/superpowers/specs/2026-11-12-drbd9-spike-results.md; grep -n "проверить на стенде" docs/superpowers/specs/2026-11-12-ambar-design.md
```

Ожидается: пустой вывод обеих команд.

- [ ] **Step 11: коммит (по команде владельца)**

```bash
git add stand/spike/ docs/
git commit -m "docs(spike): record DRBD 9 assumption checks on the stand"
```

---

## Откат

Стенд и образ не затрагивают узлы кластера.

```bash
stand/down.sh
```

```bash
docker image rm ambar-drbd:dev
```

`brew uninstall lima` удаляет Lima, если она ставилась только ради стенда.
