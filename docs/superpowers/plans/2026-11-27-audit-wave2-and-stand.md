# План реализации: исправление стенда и вторая волна улучшений аудита (`ambar`)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Починить сценарии стенда Lima (`stand/replica-e2e.sh`, `stand/failure-e2e.sh`, `stand/install.sh`) и реализовать вторую волну улучшений из аудита: `/readyz` + `resources` в DaemonSet, чтение из Reflector-кэша в `reconcile_one_locked`, `drbdsetup status <volume> --json`, полная изоляция `NodeStageVolume` в `run_detached`, проверка `read_only` при маунте, а также тюнинг `mkfs.ext4` и DRBD 9.

**Tech Stack:** Rust (Edition 2024), Tokio, Tonic, `kube`, DRBD 9.3.4 / `drbd-utils` 9.35.0, `ext4`, Lima / k3s.

## Global Constraints

- Все рассуждения, комментарии к шагам и документация — на русском языке.
- Никогда не сканировать `target/`, `.git/`, `Cargo.lock`.
- После тестов удалять промежуточные debug-артефакты (`rm -rf target/debug/deps/ambar-*`), в финале — `cargo clean`.
- Перед коммитом: `cargo fmt --check`, `cargo clippy --all-targets --no-deps -- -D warnings`, `cargo test`.

---

### Task 1: Исправление стендовых скриптов (`stand/replica-e2e.sh`, `stand/failure-e2e.sh`, `stand/install.sh`)

**Files:**
- Modify: `stand/replica-e2e.sh:234, 352, 363`
- Modify: `stand/failure-e2e.sh:271-274`
- Modify: `stand/install.sh:14-18, 143-151`

- [ ] **Step 1: Поправить `jsonpath` в `stand/replica-e2e.sh`**
  Заменить `{range .spec.nodes[*]}{if eq .kind "Witness"}{.name}{"\n"}{end}` на `{.spec.nodes[?(@.kind=="Witness")].name}` в строках 234, 352 и 363.
- [ ] **Step 2: Поправить `deployment_node` в `stand/failure-e2e.sh`**
  В `deployment_node` (строки 271-274) игнорировать удаляемые (`Terminating`) поды с `deletionTimestamp`, используя `go-template='{{range .items}}{{if not .metadata.deletionTimestamp}}{{.spec.nodeName}}{{"\n"}}{{end}}{{end}}'` и `head -n1`.
- [ ] **Step 3: Поддержка `VOLUME_GROUP` в `stand/install.sh`**
  Добавить переменную `VOLUME_GROUP="${VOLUME_GROUP:-}"` в `stand/install.sh`. Когда `VOLUME_GROUP` пуста (дефолт для Lima-стенда без LVM VG), удалять `--volume-group=k3s-vg` из `args` и `VOLUME_GROUP` из `env` при генерации манифеста DaemonSet; если `VOLUME_GROUP` задана, подставлять её вместо `k3s-vg`.
- [ ] **Step 4: Зафиксировать коммит**
  `git add stand/replica-e2e.sh stand/failure-e2e.sh stand/install.sh && git commit -m "fix(stand): fix jsonpath in replica-e2e, ignore terminating pods in failure-e2e and support sparse mode in install.sh"`

---

### Task 2: Эндпоинт `/readyz` в `metrics_server.rs` и `readinessProbe` + `resources` в `deploy/daemonset.yaml` (`M-6`)

**Files:**
- Modify: `src/agent/metrics_server.rs:15-120`
- Modify: `src/main.rs:210-250`
- Modify: `deploy/daemonset.yaml:57-92`
- Test: `src/agent/tests/metrics_server.rs`

- [ ] **Step 1: Написать тесты для `/readyz` (`200 OK` при `ready=true`, `503 Service Unavailable` при `ready=false`) в `src/agent/tests/metrics_server.rs`**
- [ ] **Step 2: Реализовать `run_metrics_server_with_ready` (сохранив `run_metrics_server` для обратной совместимости) и обработку `GET /readyz` в `src/agent/metrics_server.rs`, передать `agent.ready.clone()` в `src/main.rs`**
- [ ] **Step 3: Обновить `deploy/daemonset.yaml`**
  - Перевести `readinessProbe` на `httpGet: { path: /readyz, port: metrics }`, `periodSeconds: 5`.
  - Добавить `resources.requests` (`cpu: 20m, memory: 48Mi` для `ambar`; `cpu: 10m, memory: 24Mi` для сайдкаров).
- [ ] **Step 4: Проверить тестами и закоммитить**
  `cargo test --lib agent::metrics_server::tests`, `cargo fmt --check`, `cargo clippy --all-targets --no-deps -- -D warnings`.

---

### Task 3: Чтение из Reflector-кэша в `reconcile_one_locked` и `drbdsetup status <volume> --json` (`H-4`, `H-2`)

**Files:**
- Modify: `src/agent/kube.rs:110-160, 628-640`
- Modify: `src/agent/run.rs:830-845`
- Modify: `src/drbd/adm.rs:381-408`
- Test: `src/agent/tests/kube.rs`, `src/drbd/tests/adm.rs`

- [ ] **Step 1: Добавить `get_cached(&self, name: &str)` в трейт `Volumes` (дефолтная реализация вызывает `self.get(name)`), а в `KubeVolumes` при `self.listed.load(Ordering::SeqCst) == true` читает из `self.cache.get(&ObjectRef::new(name))`**
- [ ] **Step 2: В `Agent::reconcile_one_locked` использовать `self.volumes.get_cached(volume)` (оставив прямой `self.volumes.get` для мутирующих операций `finish_retire`, `NodeService`, `cleanup::leave_client`)**
- [ ] **Step 3: В `Drbd::status(volume)` вызывать `drbdsetup status <volume> --json` (через общий парсер `parse_statuses`) вместо `all_statuses()`**
- [ ] **Step 4: Обновить/добавить тесты в `src/drbd/tests/adm.rs` и `src/agent/tests/kube.rs`, проверить и закоммитить**

---

### Task 4: Полная изоляция `NodeStageVolume` в `run_detached` и проверка `read_only` в `mount_with_options` (`M-1`, `M-2`)

**Files:**
- Modify: `src/csi/node.rs:434-455, 982-992, 1130-1159`
- Test: `src/csi/tests/node.rs`

- [ ] **Step 1: Написать тест в `src/csi/tests/node.rs` на несоответствие `read_only` в `mount_with_options` (если том уже смонтирован `rw`, а запрашивается `ro`, возвращается `FAILED_PRECONDITION` / `Busy`)**
- [ ] **Step 2: Добавить проверку `entry.read_only != expected_ro` в `mount_with_options`**
- [ ] **Step 3: Обернуть `join` + `stage_after_join` + `teardown_after_failed_stage` в `run_detached` внутри `node_stage_volume` (убрав вложенный `run_detached` из `stage_after_join`, чтобы не брать семафор `concurrency` дважды и не ловить дедлок на семафоре!)**
- [ ] **Step 4: Проверить тестами и закоммитить**

---

### Task 5: Тюнинг `mkfs.ext4` и DRBD 9 (`M-5`), проверка на стенде Lima

**Files:**
- Modify: `src/mount.rs:235-244`
- Modify: `src/drbd/resource.rs:40-63`
- Test: `src/tests/mount.rs`, `src/drbd/tests/resource.rs`

- [ ] **Step 1: В `src/mount.rs` обновить `mkfs_ext4` на `mkfs.ext4 -q -E nodiscard,lazy_itable_init=0,lazy_journal_init=0 <device>` и обновить тест в `src/tests/mount.rs`**
- [ ] **Step 2: В `src/drbd/resource.rs` добавить буферы и настройки ресинка для NVMe + 2.5GbE (`max-buffers 8192; sndbuf-size 0; rcvbuf-size 0;` в `net`, `c-max-rate 250M;` в `disk` — проверить точный синтаксис через `drbdadm` на стенде!), обновить `src/drbd/tests/resource.rs`**
- [ ] **Step 3: Полный прогон `cargo fmt --check`, `cargo clippy --all-targets --no-deps -- -D warnings`, `cargo test`, пересборка `./image/build.sh` и сквозной прогон всех 3 скриптов стенда (`csi-e2e.sh`, `replica-e2e.sh`, `failure-e2e.sh`) + `stand/down.sh` + `cargo clean`**
