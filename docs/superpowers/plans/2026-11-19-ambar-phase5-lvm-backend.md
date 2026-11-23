# Переход ambar на LVM бэкенд и миграция NVMe кластера NanoPi R5C

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Перевести накопители NVMe 3 узлов NanoPi R5C (`k3s-1..3`) на чистый LVM2 (`k3s-vg`: системный `lv_k3s` 35 ГБ под `/var/lib/rancher` + свободные PE под блочные тома ambar), исключить loopback и двойную файловую систему, автоматизировать ввод чистых нод и снизить задержку fsync.

**Architecture:** 
1. Узлы R5C: накопитель `/dev/nvme0n1` размечается как PV `k3s-vg`, в нем создается линейный LV `lv_k3s` (35 ГБ, ext4 под `/var/lib/rancher`), а свободное пространство (~84 ГБ) отдается под прямое выделение томов `ambar` (`/dev/k3s-vg/pvc-<UUID>`).
2. ambar CSI: модуль `src/backing.rs` расширяется поддержкой LVM через CLI (`lvcreate`, `lvremove`, `lvs`), в DRBD ресурс передаются блочные устройства напрямую с внутренними метаданными (`meta-disk internal;`).
3. Безопасность: поочередная миграция нод (k3s-3 -> k3s-2 -> k3s-1) с проверкой etcd-кворума, drain и полным tar-бэкапом на eMMC перед очисткой NVMe.

**Tech Stack:** Rust 2024, Linux LVM2 (device-mapper), DRBD 9.3.4, k3s v1.36.2+, Bash.

## Global Constraints

- Все диалоги, рассуждения, коммиты и документация строго на русском языке.
- Терминология: «носитель» / «carrier» (НИКОГДА «карьер»), «гейт» / «gate» (НИКОГДА «ворота»).
- Коммиты git только с явным перечислением файлов `git commit -m "..." -- <files>`, без опций `-a`/`-A`.
- Двуязычная документация: `README.md` и `README.ru.md` обновляются строго синхронно.
- Все 902 существующих теста должны оставаться зелеными, `cargo fmt` и `cargo clippy` чистыми.

---

### Task 1: Поддержка LVM бэкенда в кодовой базе ambar

**Files:**
- Modify: `src/backing.rs`
- Modify: `src/reconcile.rs`
- Modify: `src/main.rs`
- Test: `src/tests/backing.rs`
- Test: `src/tests/reconcile.rs`

**Interfaces:**
- Consumes: `Runner` из `src/exec.rs`, `backing_file_bytes` из `src/backing.rs`.
- Produces:
  - `create_lvm_volume(runner: &dyn Runner, vg: &str, lv_name: &str, bytes: u64) -> Result<(), BackingError>`
  - `remove_lvm_volume(runner: &dyn Runner, vg: &str, lv_name: &str) -> Result<(), BackingError>`
  - `find_lvm_volume(runner: &dyn Runner, vg: &str, lv_name: &str) -> Result<bool, BackingError>`
  - `lvm_device_path(vg: &str, lv_name: &str) -> String` (`/dev/{vg}/{lv_name}`)
  - Поддержка аргумента CLI `--volume-group=<VG>` в `ambar agent`.

- [ ] **Step 1: Написать падающий юнит-тест для операций LVM**

В `src/tests/backing.rs`:
```rust
#[test]
fn test_lvm_create_and_remove() {
    let runner = FakeRunner::new();
    runner.push("lvs --noheadings -o lv_name /dev/k3s-vg/pvc-test", Ok("pvc-test\n".into()));
    assert!(crate::backing::find_lvm_volume(&runner, "k3s-vg", "pvc-test").unwrap());

    runner.push("lvcreate -y -L 1073741824B -n pvc-test k3s-vg", Ok("".into()));
    crate::backing::create_lvm_volume(&runner, "k3s-vg", "pvc-test", 1073741824).unwrap();

    runner.push("lvremove -y /dev/k3s-vg/pvc-test", Ok("".into()));
    crate::backing::remove_lvm_volume(&runner, "k3s-vg", "pvc-test").unwrap();
}
```

- [ ] **Step 2: Запустить тесты и убедиться в ошибке компиляции**

Команда: `cargo test --test backing test_lvm_create_and_remove`
Ожидается: ошибка компиляции `no function or associated item named find_lvm_volume`.

- [ ] **Step 3: Реализовать функции LVM в `src/backing.rs`**

В `src/backing.rs` добавить реализацию вызовов `lvs`, `lvcreate`, `lvremove`:
```rust
pub fn lvm_device_path(vg: &str, lv_name: &str) -> String {
    format!("/dev/{vg}/{lv_name}")
}

pub fn find_lvm_volume(runner: &dyn Runner, vg: &str, lv_name: &str) -> Result<bool, BackingError> {
    let dev = lvm_device_path(vg, lv_name);
    let out = runner.run("lvs", &["--noheadings", "-o", "lv_name", &dev]);
    match out {
        Ok(_) => Ok(true),
        Err(e) if e.to_string().contains("not found") || e.to_string().contains("Failed to find") => Ok(false),
        Err(e) => Err(BackingError::Spawn { command: "lvs".into(), message: e.to_string() }),
    }
}

pub fn create_lvm_volume(runner: &dyn Runner, vg: &str, lv_name: &str, bytes: u64) -> Result<(), BackingError> {
    let size_arg = format!("{bytes}B");
    checked(runner, "lvcreate", &["-y", "-L", &size_arg, "-n", lv_name, vg])?;
    Ok(())
}

pub fn remove_lvm_volume(runner: &dyn Runner, vg: &str, lv_name: &str) -> Result<(), BackingError> {
    let dev = lvm_device_path(vg, lv_name);
    checked(runner, "lvremove", &["-y", &dev])?;
    Ok(())
}
```

- [ ] **Step 4: Интегрировать LVM в `src/reconcile.rs` и CLI аргументы в `src/main.rs`**

- Добавить поле `volume_group: Option<String>` в конфигурацию Reconciler.
- При наличии `volume_group`: использовать LVM блочное устройство `/dev/<vg>/<vol>` напрямую в конфигурации `.res` вместо loopback симлинка.

- [ ] **Step 5: Запустить полный набор тестов и проверить clippy/fmt**

Команда: `cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check`
Ожидается: все тесты зеленые, 0 предупреждений.

- [ ] **Step 6: Закоммитить изменения Task 1**

```bash
git add src/backing.rs src/reconcile.rs src/main.rs src/tests/
git commit -m "feat(storage): implement direct LVM backend for DRBD volumes" -- src/backing.rs src/reconcile.rs src/main.rs src/tests/
```

---

### Task 2: Поддержка LVM в Dockerfile и манифестах Kubernetes

**Files:**
- Modify: `image/Dockerfile:46-59`
- Modify: `deploy/daemonset.yaml`

**Interfaces:**
- Produces: Docker образ `ambar-drbd:dev` с установленным `lvm2` и манифест `daemonset.yaml` с монтированием `/run/lvm` и аргументом `--volume-group`.

- [ ] **Step 1: Добавить `lvm2` в `image/Dockerfile`**

В `image/Dockerfile` в секцию рантайма:
```dockerfile
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      kmod util-linux e2fsprogs libkeyutils1 lvm2 \
 && rm -rf /var/lib/apt/lists/*
```

- [ ] **Step 2: Обновить монтирования в `deploy/daemonset.yaml`**

Добавить volume и volumeMount для `/run/lvm`:
```yaml
            - {name: run-lvm, mountPath: /run/lvm}
      volumes:
        - name: run-lvm
          hostPath: {path: /run/lvm, type: DirectoryOrCreate}
```

- [ ] **Step 3: Проверить валидность манифеста**

Команда: `kubectl apply --dry-run=client -f deploy/daemonset.yaml`
Ожидается: `daemonset.apps/ambar configured (dry run)`.

- [ ] **Step 4: Закоммитить изменения Task 2**

```bash
git add image/Dockerfile deploy/daemonset.yaml
git commit -m "build(docker): include lvm2 and host /run/lvm mount in ambar daemonset" -- image/Dockerfile deploy/daemonset.yaml
```

---

### Task 3: Скрипты первичной разметки чистой ноды и накатки k3s с нуля

**Files:**
- Create: `stand/r5c-init-node.sh`
- Create: `stand/r5c-install-k3s.sh`
- Modify: `stand/r5c-preflight.sh`

**Interfaces:**
- Produces:
  - `stand/r5c-init-node.sh <node>` — подготовка LVM `k3s-vg` и `lv_k3s` на голом узле.
  - `stand/r5c-install-k3s.sh <node> [first_ip token]` — установка k3s на чистый LVM.
  - `stand/r5c-preflight.sh` — проверка наличия `k3s-vg`, свободного места $\ge 50$ ГБ и статуса `lv_k3s`.

- [ ] **Step 1: Написать `stand/r5c-init-node.sh`**

Скрипт принимает имя узла (`k3s-1..3`), устанавливает `lvm2`, очищает NVMe, создает PV `/dev/nvme0n1`, VG `k3s-vg`, LV `lv_k3s` 35 ГБ, форматирует `mkfs.ext4 -L k3s-data`, прописывает `/etc/fstab` и монтирует `/var/lib/rancher`.

- [ ] **Step 2: Написать `stand/r5c-install-k3s.sh`**

Скрипт проверяет точку монтирования `/var/lib/rancher`, запускает установку k3s server с отключением servicelb/traefik и привязкой к IP на `lan0`.

- [ ] **Step 3: Обновить `stand/r5c-preflight.sh`**

Добавить шаг валидации LVM:
```bash
echo "=== 4. Checking LVM topology (k3s-vg / lv_k3s) ==="
for n in "${R5C_NODES[@]}"; do
    r5c_ssh "$n" "vgs k3s-vg --noheadings -o vg_free --units g | awk '{if (\$1 < 40) exit 1}'"
    r5c_ssh "$n" "lvs /dev/k3s-vg/lv_k3s >/dev/null"
done
```

- [ ] **Step 4: Проверить синтаксис bash-скриптов**

Команда: `bash -n stand/r5c-init-node.sh stand/r5c-install-k3s.sh stand/r5c-preflight.sh`
Ожидается: код выхода 0.

- [ ] **Step 5: Закоммитить изменения Task 3**

```bash
git add stand/r5c-init-node.sh stand/r5c-install-k3s.sh stand/r5c-preflight.sh
git commit -m "feat(stand): add scripts for fresh node LVM partitioning and k3s bootstrap" -- stand/r5c-init-node.sh stand/r5c-install-k3s.sh stand/r5c-preflight.sh
```

---

### Task 4: Скрипт безопасной понодовой миграции кластера на LVM

**Files:**
- Create: `stand/r5c-migrate-lvm.sh`

**Interfaces:**
- Produces: `stand/r5c-migrate-lvm.sh <node>` — выполняет 8 шагов регламента миграции узла (cordon, drain, stop k3s, tar backup, LVM partition, mount, tar restore, start k3s, uncordon).

- [ ] **Step 1: Реализовать `stand/r5c-migrate-lvm.sh`**

Скрипт с пошаговой валидацией:
- Проверка etcd кворума через `k3s kubectl get nodes`.
- Выполнение `drain`.
- Создание архива на eMMC: `tar --sparse -czf /root/k3s-backup-${node}.tar.gz /var/lib/rancher`.
- Верификация `tar -tzf`.
- Размонтирование `/var/lib/rancher`.
- Инициализация LVM (`pvcreate -f /dev/nvme0n1`, `vgcreate k3s-vg /dev/nvme0n1`, `lvcreate -L 35G -n lv_k3s k3s-vg`, `mkfs.ext4 -L k3s-data /dev/k3s-vg/lv_k3s`).
- Обновление `/etc/fstab` на `/dev/k3s-vg/lv_k3s`.
- Монтирование `/var/lib/rancher`.
- Восстановление `tar -xzf /root/k3s-backup-${node}.tar.gz -C /`.
- Запуск `systemctl start k3s` и ожидание готовности узла.
- `uncordon`.

- [ ] **Step 2: Проверить синтаксис скрипта**

Команда: `bash -n stand/r5c-migrate-lvm.sh`
Ожидается: код выхода 0.

- [ ] **Step 3: Закоммитить скрипт миграции**

```bash
git add stand/r5c-migrate-lvm.sh
git commit -m "feat(stand): add automated safe node migration script to LVM" -- stand/r5c-migrate-lvm.sh
```

---

### Task 5: Выполнение поочередной миграции физических узлов кластера

**Files:**
- Боевой кластер: узлы `k3s-3`, `k3s-2`, `k3s-1`.

**Interfaces:**
- Consumes: `stand/r5c-migrate-lvm.sh`, физические накопители NVMe.
- Produces: Все 3 узла работают с `/var/lib/rancher` поверх LVM `lv_k3s`, в `k3s-vg` свободно ~84 ГБ на каждом узле.

- [ ] **Step 1: Установить `lvm2` на всех трёх узлах**

Команда: `source stand/r5c-lib.sh && for n in "${R5C_NODES[@]}"; do r5c_ssh "$n" "apt-get update && apt-get install -y lvm2"; done`
Ожидается: пакет `lvm2` установлен на всех 3 узлах.

- [ ] **Step 2: Выполнить миграцию пилотного узла `k3s-3`**

Команда: `./stand/r5c-migrate-lvm.sh k3s-3`
Ожидается: узел k3s-3 смигрирован, k3s запущен, etcd кворум 3/3, статус Ready.

- [ ] **Step 3: Выполнить миграцию узла `k3s-2`**

Команда: `./stand/r5c-migrate-lvm.sh k3s-2`
Ожидается: узел k3s-2 смигрирован, статус Ready.

- [ ] **Step 4: Выполнить миграцию узла `k3s-1`**

Команда: `./stand/r5c-migrate-lvm.sh k3s-1`
Ожидается: узел k3s-1 смигрирован, поды пользовательских workload'ов восстановили работу, статус Ready.

- [ ] **Step 5: Проверить статус кластера и свободное место LVM**

Команда: `kubectl get nodes -o wide && source stand/r5c-lib.sh && for n in "${R5C_NODES[@]}"; do echo "=== $n ==="; r5c_ssh "$n" "vgs k3s-vg && lvs"; done`
Ожидается: все 3 узла Ready, `k3s-vg` имеет ~84 ГБ свободного пространства.

---

### Task 6: Развертывание ambar с LVM бэкендом и замер задержки fsync

**Files:**
- Modify: `stand/bench-fsync-r3-lvm.json` (новые результаты)
- Modify: `stand/bench-fsync-r2-lvm.json`

**Interfaces:**
- Consumes: обновленный образ `ambar-drbd:dev`, тестовый сценарий `stand/r5c-bench-fsync.sh`.
- Produces: Замеры задержки 4K fsync на чистом блочном устройстве LVM DRBD.

- [ ] **Step 1: Собрать и импортировать обновленный образ ambar**

Собрать образ через `docker build` и импортировать на узлы R5C.

- [ ] **Step 2: Развернуть DaemonSet с параметром `--volume-group=k3s-vg`**

Команда: `stand/r5c-deploy.sh`
Ожидается: 3/3 подов `ambar` в статусе Running.

- [ ] **Step 3: Запустить тест fsync на томе LVM**

Команда: `./stand/r5c-bench-fsync.sh`
Ожидается: успешное завершение fio, фиксация задержек `p50`, `p95`, `p99`.

- [ ] **Step 4: Зафиксировать результаты замеров в репозитории**

```bash
git add stand/bench-fsync-*.json
git commit -m "bench(r5c): record fsync 4K benchmark results on direct LVM backend" -- stand/bench-fsync-*.json
```

---

### Task 7: Синхронное обновление документации

**Files:**
- Modify: `README.md`
- Modify: `README.ru.md`
- Modify: `docs/superpowers/specs/2026-11-19-ambar-phase5-lvm-backend.md` (добавление фактических замеров)
- Modify: `проектную документацию`

**Interfaces:**
- Produces: Актуальная двуязычная документация с описанием архитектуры LVM и инструкциями разметки узлов с нуля.

- [ ] **Step 1: Обновить `README.md` (EN)**

Описать требования к диску: LVM Volume Group `k3s-vg`, раздел `lv_k3s` 35 ГБ под `/var/lib/rancher`, остаток под тома ambar. Обновить секцию Getting Started.

- [ ] **Step 2: Синхронно обновить `README.ru.md` (RU)**

Зеркально портировать все изменения на русский язык, соблюдая закрепленные термины («носитель», «гейт»).

- [ ] **Step 3: Зафиксировать факт замеров в протоколе и памяти Claude**

Обновить `docs/superpowers/specs/2026-11-19-ambar-phase5-lvm-backend.md` реальными цифрами fsync и обновить `MEMORY.md`.

- [ ] **Step 4: Закоммитить документацию Task 7**

```bash
git add README.md README.ru.md docs/superpowers/specs/
git commit -m "docs: document LVM storage topology and fresh node bootstrap instructions" -- README.md README.ru.md docs/superpowers/specs/
```
