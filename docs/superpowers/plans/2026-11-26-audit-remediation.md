# План реализации по итогам архитектурного и кодового аудита (`ambar`)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Устранить все выявленные в ходе архитектурного и кодового аудита проблемы производительности, конкурентности, учёта ёмкости LVM, взаимодействия с Kubernetes API, надёжности DRBD/LVM и конфигурации сборки (`C-1..2`, `H-1..6`, `M-1..6`, `L-1..5`).

**Architecture:**
1. Перевод `SystemRunner` (`src/exec.rs`) с двух потоков `std::thread::spawn` и 20-мс поллинга `sleep` на однопоточный неблокирующий мультиплексор `libc::poll` с принудительной локалью `LC_ALL=C` и фоновым добором отложенных зомби-процессов.
2. Оптимизация горячих циклов `Reconciler` и `Agent`: возврат прочитанного `ResourceStatus` из `Reconciler::apply` (устранение двойного вызова `drbdsetup status`), fast-path проверка `/dev/<vg>/<volume>` перед запуском `lvs`, пакетное кэширование `PVC` при проверке подов узла, исключение лишнего `volumes.get` в `replica_cmd`.
3. Устранение гонок и блокировок: защита активных `NodeStageVolume` от фоновой очистки `cleanup.rs` через реестр `staging_volumes` в `Agent`, переход на неблокирующий `try_lock` в `tick_failures`, атомарная проверка `prove_pending_metadata` под `flock` с реальным `minor` тома, перенос `join` и `wait_usable` под `run_detached` в `NodeService`.
4. Точный расчёт бюджетов и управление репликами: хранение `max_peers` в `Existing`, учёт метаданных DRBD и LVM PE в `free_budgets_aligned`, отслеживание попыток `StandaloneTracker` по пирам, проверка коллизий `node_id`/`address` и `Some(from)` на втором шаге `next_move` в `replicas.rs`, а также оптимизации аллокаций, метрик, `[profile.release]` и `deploy/`.

**Tech Stack:** Rust (Edition 2024), Tokio, Tonic (CSI gRPC), `kube` / `k8s-openapi`, `libc` (`poll`, `fcntl`, `waitid`), DRBD 9 (`drbdadm`, `drbdsetup`), LVM2, `tikv-jemallocator`.

## Global Constraints

- Все рассуждения, комментарии к шагам и документация — на русском языке; двуязычная документация (`*.md` и `*.ru.md`) обновляется синхронно.
- Никогда не сканировать `target/`, `.git/`, `Cargo.lock`.
- После выполнения тестов на каждом шаге обязательно удалять промежуточные debug-артефакты (`rm -rf target/debug/deps/ambar-* target/debug/ambar`), а в конце — `cargo clean`.
- Перед каждым коммитом выполнять проверки:
  - `cargo fmt --check`
  - `cargo clippy --all-targets --no-deps -- -D warnings`
  - `cargo test`
- При структурных изменениях в `src/` обновлять индекс `python3 scripts/code-index.py`.

---

### Task 1: Однопоточный `poll(2)` без `thread::spawn` и `sleep(20ms)`, `LC_ALL=C` и добор D-state зомби в `SystemRunner` (`C-1`, `H-2`, `H-5`, `M-6`)

**Files:**
- Modify: `src/exec.rs:1-395`
- Test: `src/tests/exec.rs`

**Interfaces:**
- Consumes: `libc::{poll, pollfd, POLLIN, POLLHUP, POLLERR, fcntl, F_GETFL, F_SETFL, O_NONBLOCK, waitid, P_PID, WEXITED, WNOHANG, WNOWAIT}`
- Produces: `SystemRunner::run_with_timeout` (публичный трейт `Runner` и сигнатуры `Output`, `command_line`, `MAX_OUTPUT_CAPTURE` остаются неизменными).

- [ ] **Step 1: Написать тесты на скорость выполнения быстрой команды (< 15 мс без 20-мс поллинга), принудительную локаль `LC_ALL=C` и добор отложенных зомби**

Добавить в конец `src/tests/exec.rs`:

```rust
#[test]
fn fast_command_finishes_without_poll_interval_sleep() {
    let runner = SystemRunner;
    let start = Instant::now();
    let out = runner.run("true", &[]).unwrap();
    assert!(out.success());
    // Без безусловного thread::sleep(20ms) `true` завершается за единицы миллисекунд.
    assert!(
        start.elapsed() < Duration::from_millis(18),
        "fast command took too long: {:?}",
        start.elapsed()
    );
}

#[test]
fn command_runs_with_c_locale() {
    let runner = SystemRunner;
    let out = runner.run("sh", &["-c", "printf '%s' \"$LC_ALL\""]).unwrap();
    assert!(out.success());
    assert_eq!(out.stdout, "C");
}
```

- [ ] **Step 2: Запустить новые тесты и убедиться, что они падают**

Run: `cargo test --lib exec::tests::command_runs_with_c_locale exec::tests::fast_command_finishes_without_poll_interval_sleep`
Expected: FAIL (`command_runs_with_c_locale` получает пустую или системную локаль вместо `"C"`, `fast_command_finishes_without_poll_interval_sleep` занимает `>= 20ms`).

- [ ] **Step 3: Реализовать неблокирующее чтение `stdout`/`stderr` через `libc::poll` в одном потоке и `.env("LC_ALL", "C")` в `src/exec.rs`**

В `src/exec.rs`:
1. В `Command::new(program)` добавить `.env("LC_ALL", "C")`.
2. Перевести `ChildStdout` и `ChildStderr` в `O_NONBLOCK` через `libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK)`.
3. Заменить `drain` (`thread::spawn` + `mpsc::channel`) и `thread::sleep(POLL_INTERVAL)` в `supervise` на единый цикл `libc::poll` по двум `pollfd` (`stdout`, `stderr`) с шагом ожидания `poll_timeout_ms` (например, `5ms` пока `exited_without_reaping(group)` возвращает `false`, а при закрытии обоих пайпов `POLLHUP`/EOF — немедленная проверка `exited_without_reaping(group)`).
4. Как только `exited_without_reaping(group)` вернул `Ok(true)`, дочитать оставшиеся данные из открытых пайпов через `poll` с дедлайном `deadline.min(Instant::now() + READER_GRACE)` (если фоновый потомок продолжает удерживать пайп открытым дольше дедлайна, закрыть дескрипторы пайпов, вызвать `kill_and_reap(child, group)` и вернуть `timeout_error` с накопленным `stderr`).
5. При чтении каждого пайпа ограничивать буфер `MAX_OUTPUT_CAPTURE` (1 МиБ), а лишние байты сливать в пустой буфер `[0u8; 8192]` с добавлением `TRUNCATED_SUFFIX`, чтобы дочерний процесс никогда не получал `SIGPIPE` и не блокировался на переполнении буфера пайпа.
6. Хранить список несобранных после `REAP_LIMIT` PID в `static UNREAPED_PIDS: Mutex<Vec<u32>> = Mutex::new(Vec::new());` и в начале `run_with_timeout` вызывать неблокирующий `reap_lingering_zombies()` (`libc::waitpid(pid as i32, std::ptr::null_mut(), libc::WNOHANG) > 0`), чтобы вышедшие позже из `D-state` процессы автоматически убирались из таблицы процессов.

- [ ] **Step 4: Запустить весь набор тестов `exec::tests`**

Run: `cargo test --lib exec::tests && rm -rf target/debug/deps/ambar-*`
Expected: PASS (все существующие и новые тесты в `src/tests/exec.rs` проходят зелёными).

- [ ] **Step 5: Зафиксировать коммит**

```bash
cargo fmt --check
cargo clippy --all-targets --no-deps -- -D warnings
git add src/exec.rs src/tests/exec.rs
git commit -m "perf(exec): replace reader threads and 20ms sleep with single-threaded poll"
```

---

### Task 2: Устранение повторного вызова `drbdsetup status` в `Agent::apply` и fast-path проверка LVM-устройства в `Reconciler::observe` (`H-2`)

**Files:**
- Modify: `src/reconcile.rs:372-445`
- Modify: `src/agent/run.rs:921-981`
- Test: `src/tests/reconcile.rs`, `src/agent/tests/run.rs`

**Interfaces:**
- Produces:
  - `Reconciler::apply_with_status(&self, spec: &VolumeSpec, node_name: &str) -> Result<(Vec<Action>, Option<ResourceStatus>), ReconcileError>`
  - `Reconciler::apply(&self, spec: &VolumeSpec, node_name: &str) -> Result<Vec<Action>, ReconcileError>` (тонкая обёртка над `apply_with_status`, сохраняющая обратную совместимость со всеми существующими тестами и CLI `ambar apply`).

- [ ] **Step 1: Написать тесты на отсутствие повторного `drbdsetup status` в `Agent::apply` и fast-path проверку `/dev/<vg>/<volume>` в `Reconciler::observe`**

1. В `src/tests/reconcile.rs` добавить тест, проверяющий, что при наличии блочного устройства `/dev/<vg>/<volume>` на диске `Reconciler::observe` определяет `FileState::Ready` без вызова внешней команды `lvs` (и вызывает `lvs` только если файл `/dev/<vg>/<volume>` отсутствует):
```rust
#[test]
fn observe_with_volume_group_uses_device_stat_fast_path_when_present() {
    let tmp = TempDir::new().unwrap();
    let paths = temp_paths(&tmp);
    let vg_dir = tmp.path().join("dev/k3s-vg");
    fs::create_dir_all(&vg_dir).unwrap();
    let spec = spec();
    // Когда LV не существует на диске, вызывается fallback `lvs`
    let runner = FakeRunner::new(|prog, _| match prog {
        "lvs" => out(5, "", "  Failed to find logical volume \"k3s-vg/spike\"\n"),
        "drbdsetup" => out(0, "[]\n", ""),
        other => panic!("unexpected call: {other}"),
    });
    let rec = Reconciler::with_volume_group(&paths, &runner, Some("k3s-vg".into()));
    let obs = rec.observe(&spec, "lima-dv-1").unwrap();
    assert_eq!(obs.file, FileState::Absent);
}
```
2. В `src/agent/tests/run.rs` добавить тест, проверяющий, что на уже сконфигурированном (`settled`) томе один проход `reconcile_one` вызывает `drbdsetup status --json` ровно **1 раз** (внутри `observe`), а не 2 раза:
```rust
#[tokio::test]
async fn settled_volume_calls_drbdsetup_status_only_once_per_pass() {
    let e = configured_env().await;
    e.agent.reconcile_one(VOLUME).await.unwrap();
    let before = e.commands().len();
    let outcome = e.agent.reconcile_one(VOLUME).await.unwrap();
    assert_eq!(outcome, Outcome::Settled);
    let after = e.commands();
    let drbdsetup_calls = after[before..]
        .iter()
        .filter(|c| c.starts_with("drbdsetup status"))
        .count();
    assert_eq!(drbdsetup_calls, 1, "calls in settled pass: {:?}", &after[before..]);
}
```

- [ ] **Step 2: Запустить тест и убедиться, что `settled_volume_calls_drbdsetup_status_only_once_per_pass` падает**

Run: `cargo test --lib agent::tests::run::settled_volume_calls_drbdsetup_status_only_once_per_pass`
Expected: FAIL (`assertion left == right failed: left: 2, right: 1`).

- [ ] **Step 3: Реализовать `Reconciler::apply_with_status` и fast-path `exists` для LVM в `src/reconcile.rs` и `src/agent/run.rs`**

1. В `src/reconcile.rs`:
```rust
    pub fn apply(&self, spec: &VolumeSpec, node_name: &str) -> Result<Vec<Action>, ReconcileError> {
        self.apply_with_status(spec, node_name).map(|(actions, _)| actions)
    }

    pub fn apply_with_status(
        &self,
        spec: &VolumeSpec,
        node_name: &str,
    ) -> Result<(Vec<Action>, Option<ResourceStatus>), ReconcileError> {
        spec.validate()?;
        let _lock = self.lock_volume(&spec.name)?;
        let mut done = Vec::new();
        let mut last_status = None;
        let mut status_dirty = true;
        for _ in 0..MAX_ROUNDS {
            let obs = self.observe(spec, node_name)?;
            last_status = obs.status.clone();
            status_dirty = false;
            let actions = plan(spec, node_name, &obs)?;
            if actions.is_empty() {
                break;
            }
            let mut loop_device = obs.loop_device.clone();
            for action in &actions {
                self.execute(spec, node_name, action, &mut loop_device)?;
            }
            status_dirty = true;
            let skipped = actions.contains(&Action::SkipInitialSync);
            done.extend(actions);
            if skipped {
                break;
            }
        }
        if status_dirty {
            last_status = Drbd { runner: self.runner }.status(&spec.name)?;
        }
        Ok((done, last_status))
    }
```
2. В `Reconciler::observe` (`src/reconcile.rs:L401-L404`):
```rust
        let backing_exists = match &self.volume_group {
            Some(vg) => {
                let dev_path = backing::lvm_device_path(vg, volume);
                if exists(Path::new(&dev_path))? {
                    true
                } else {
                    backing::find_lvm_volume(self.runner, vg, volume)?
                }
            }
            None => exists(&image)?,
        };
```
3. В `Agent::apply` (`src/agent/run.rs:L930-L961`) вызывать `.apply_with_status(&spec, &node)` и получать `(actions, status)` за один заход без повторного `self.read_status(volume).await`.

- [ ] **Step 4: Запустить тесты `reconcile` и `agent::tests::run`**

Run: `cargo test --lib reconcile agent::tests::run && rm -rf target/debug/deps/ambar-*`
Expected: PASS.

- [ ] **Step 5: Зафиксировать коммит**

```bash
cargo fmt --check
cargo clippy --all-targets --no-deps -- -D warnings
git add src/reconcile.rs src/agent/run.rs src/tests/reconcile.rs src/agent/tests/run.rs
git commit -m "perf(reconcile): reuse observed DRBD status in Agent::apply and fast-path LVM stat"
```

---

### Task 3: Защита `NodeStageVolume` от гонки с `cleanup.rs`, неблокирующий `try_lock` в `tick_failures` и изоляция `NodeStageVolume` в `run_detached` (`C-2`, `H-1`, `M-1`, `M-6`)

**Files:**
- Modify: `src/agent/run.rs:270-340, 1730-1800, 1840-1865`
- Modify: `src/agent/cleanup.rs:25-45, 97-146`
- Modify: `src/csi/node.rs:680-710, 982-992, 1103-1160`
- Test: `src/agent/tests/cleanup.rs`, `src/csi/tests/node.rs`

**Interfaces:**
- Produces:
  - `Agent::begin_stage(&self, volume: &str) -> StageGuard` и `Agent::is_staging(&self, volume: &str) -> bool`
  - `CleanupTracker::with_initial_delay(interval: Duration, now: Instant) -> Self`

- [ ] **Step 1: Написать тесты на защиту от очистки `Client` во время `NodeStageVolume`, неблокирующий `try_lock` в `cleanup` и сохранение ошибки `pass()` в `wait_usable`**

1. В `src/agent/tests/cleanup.rs` добавить тесты:
```rust
#[tokio::test]
async fn cleanup_skips_volume_while_stage_is_in_progress() {
    let e = crate::agent::tests::run::env().await;
    let spec = spec_with_nodes(vec![
        member("dv-1", 0, NodeKind::Replica),
        member("dv-2", 1, NodeKind::Replica),
        member(&e.agent.node, 2, NodeKind::Client),
    ]);
    let obj = ReplicatedVolume::new("spike", spec);
    e.volumes.put(obj.clone());

    let _stage_guard = e.agent.begin_stage("spike");
    let res = crate::agent::cleanup::check_and_cleanup_client_with_snapshot(
        &e.agent,
        "spike",
        &obj,
        None,
        &[],
    )
    .await
    .unwrap();
    assert_eq!(res, None);
    let current = e.volumes.get("spike").await.unwrap().unwrap();
    assert!(current.spec.nodes.iter().any(|n| n.name == e.agent.node));
}

#[tokio::test]
async fn cleanup_skips_volume_when_volume_lock_is_held() {
    let e = crate::agent::tests::run::env().await;
    let spec = spec_with_nodes(vec![
        member("dv-1", 0, NodeKind::Replica),
        member("dv-2", 1, NodeKind::Replica),
        member(&e.agent.node, 2, NodeKind::Client),
    ]);
    let obj = ReplicatedVolume::new("spike", spec);
    e.volumes.put(obj.clone());

    let _held = e.agent.locks.lock("spike").await;
    // Должен мгновенно вернуть Ok(None), не блокируясь на локе
    let res = tokio::time::timeout(
        Duration::from_millis(50),
        crate::agent::cleanup::check_and_cleanup_client_with_snapshot(&e.agent, "spike", &obj, None, &[]),
    )
    .await
    .expect("cleanup must not block on held volume lock")
    .unwrap();
    assert_eq!(res, None);
}
```

- [ ] **Step 2: Запустить новые тесты и убедиться, что они не компилируются / падают**

Run: `cargo test --lib agent::tests::cleanup::cleanup_skips_volume`
Expected: FAIL (`no method named begin_stage found for struct Agent`).

- [ ] **Step 3: Реализовать `StageGuard`, `try_lock` в `tick_failures`/`cleanup`, `MissedTickBehavior::Delay` и диагностику в `wait_usable`**

1. В `src/agent/run.rs`:
   - Добавить в `struct Agent` поле `pub staging: Arc<Mutex<HashMap<String, usize>>>,` (инициализировать `Arc::new(Mutex::new(HashMap::new()))`).
   - Реализовать `pub struct StageGuard { staging: Arc<Mutex<HashMap<String, usize>>>, volume: String }` с `Drop` (декремент счётчика и удаление ключа при `0`), а также методы `Agent::begin_stage(&self, volume: &str) -> StageGuard` и `Agent::is_staging(&self, volume: &str) -> bool`.
   - В `tick_failures` на шаге 3 (`StandAlone` adjust, `src/agent/run.rs:L1739`): заменить `let _lock = self.locks.lock(&name).await;` на:
     ```rust
     let Some(_lock) = self.locks.try_lock(&name) else {
         continue;
     };
     ```
   - В `run_failure_loop` (`src/agent/run.rs:L1846`):
     ```rust
     let mut interval = tokio::time::interval(Duration::from_secs(2));
     interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
     ```
2. В `src/agent/cleanup.rs`:
   - В `check_and_cleanup_client_with_snapshot` (`src/agent/cleanup.rs:L104`):
     ```rust
     if !crate::model::valid_volume_name(volume) || agent.is_staging(volume) {
         return Ok(None);
     }
     ```
   - На строке `L120` заменить `let _lock = agent.locks.lock(volume).await;` на:
     ```rust
     let Some(_lock) = agent.locks.try_lock(volume) else {
         return Ok(None);
     };
     if agent.is_staging(volume) {
         return Ok(None);
     }
     ```
3. В `src/csi/node.rs`:
   - В `node_stage_volume` захватывать `let _stage_guard = self.agent.begin_stage(&volume);` перед `self.join(&volume).await?`.
   - В `wait_usable` (`src/csi/node.rs:L683-L709`) сохранять текст последней ошибки `pass(volume)` (`mut last_pass_err: Option<String> = None`) и добавлять его к сообщению об ошибке при наступлении `deadline`, чтобы причина сбоя реконсиляции не терялась.

- [ ] **Step 4: Запустить тесты `cleanup` и `csi::tests::node`**

Run: `cargo test --lib agent::tests::cleanup csi::tests::node && rm -rf target/debug/deps/ambar-*`
Expected: PASS.

- [ ] **Step 5: Зафиксировать коммит**

```bash
cargo fmt --check
cargo clippy --all-targets --no-deps -- -D warnings
git add src/agent/run.rs src/agent/cleanup.rs src/csi/node.rs src/agent/tests/cleanup.rs src/main.rs
git commit -m "fix(agent,csi): guard NodeStageVolume against stale client cleanup and use try_lock in failure loop"
```

---

### Task 4: Безопасная проверка метаданных `prove_pending_metadata` под `flock` с реальным `minor` и очистка `.tmp` файлов (`H-6`, `M-2`, `M-5`)

**Files:**
- Modify: `src/agent/run.rs:475-575, 1040-1105, 1255-1380`
- Modify: `src/reconcile.rs:670-705, 785-815`
- Test: `src/agent/tests/run.rs`, `src/tests/reconcile.rs`

**Interfaces:**
- Produces:
  - `prove_no_data(paths: &Paths, runner: &dyn Runner, volume: &str, minor: u32, file: FileState, volume_group: Option<&str>) -> Result<(), String>`
  - `finish_retire` с циклом ретраев `0..EDIT_ATTEMPTS` при конфликте `replace_members`.

- [ ] **Step 1: Написать тесты на использование `spec.minor` в `prove_pending_metadata`, ретрай `finish_retire` при конфликте и очистку `.tmp` файлов в `remove_guarded`**

1. В `src/tests/reconcile.rs` добавить тест, проверяющий, что `remove_checked` / `remove_unconfigured` удаляет также осиротевшие временные файлы `.<volume>.res.tmp`, `.<volume>.img.ready.tmp` и `.<volume>.tmp`:
```rust
#[test]
fn remove_cleans_up_leftover_atomic_temp_files() {
    let tmp = TempDir::new().unwrap();
    let paths = temp_paths(&tmp);
    let runner = FakeRunner::new(|prog, _| match prog {
        "drbdsetup" => out(0, "[]\n", ""),
        "losetup" => out(0, "", ""),
        other => panic!("unexpected: {other}"),
    });
    let rec = Reconciler::new(&paths, &runner);
    let res_tmp = paths.res_file("spike").with_file_name(".spike.res.tmp");
    let ready_tmp = paths.ready_marker("spike").with_file_name(".spike.img.ready.tmp");
    fs::create_dir_all(res_tmp.parent().unwrap()).unwrap();
    fs::create_dir_all(ready_tmp.parent().unwrap()).unwrap();
    fs::write(&res_tmp, "partial").unwrap();
    fs::write(&ready_tmp, "").unwrap();

    rec.remove_unconfigured("spike", &|| Ok(())).unwrap();
    assert!(!res_tmp.exists());
    assert!(!ready_tmp.exists());
}
```
2. В `src/agent/tests/run.rs` добавить тест на то, что `prove_pending_metadata` записывает в dummy `.res` реальный `minor` тома (например, `1000`), а не `0`, и что `finish_retire` успешно завершает удаление `Retiring`-узла из `spec.nodes`, даже если между началом `retire_replica` и `finish_retire` другой узел обновил `spec.nodes`.

- [ ] **Step 2: Запустить тесты и убедиться, что они падают**

Run: `cargo test --lib reconcile::tests::remove_cleans_up_leftover_atomic_temp_files`
Expected: FAIL (`assert!(!res_tmp.exists())` падает).

- [ ] **Step 3: Реализовать атомарную запись с `spec.minor` под `flock`, ретраи в `finish_retire` и очистку `.tmp`-файлов**

1. В `src/agent/run.rs`:
   - Передавать `minor: u32` (`object.spec.minor`) в `prove_no_data` и `prove_pending_metadata`.
   - В `prove_pending_metadata` (`src/agent/run.rs:L548-L555`) формировать `device /dev/drbd{minor};` и записывать файл атомарно через временный файл `.<volume>.res.tmp` + `fs::rename`.
   - В `delete_replica` (`src/agent/run.rs:L1044-L1054`) и `retire_replica` (`src/agent/run.rs:L1257-L1267`) не вызывать `prove_no_data` без `flock` до `remove_unconfigured`: просто переходить к `Check::NoData(file)` / `CheckRetire::NoData(file)`, где `remove_unconfigured` сначала берёт `lock_volume` (`flock`) и затем вызывает `guard` (`prove_no_data`).
   - В `finish_retire` (`src/agent/run.rs:L1358-L1377`) обернуть чтение свежего `self.volumes.get(volume)` и вызов `replace_members` в цикл `for attempt in 0..EDIT_ATTEMPTS` с `conflict_backoff(attempt)`.
2. В `src/reconcile.rs`:
   - В `remove_guarded` (`src/reconcile.rs:L688-L699`) дополнительно вызывать `remove_if_present` для `temp_path(&self.paths.res_file(volume))`, `temp_path(&self.paths.applied_res(volume))`, `temp_path(&self.paths.ready_marker(volume))`, `temp_path(&self.paths.synced_marker(volume))` и `temp_path(&self.paths.device_link(volume))`.

- [ ] **Step 4: Запустить тесты `reconcile` и `agent::tests::run`**

Run: `cargo test --lib reconcile agent::tests::run && rm -rf target/debug/deps/ambar-*`
Expected: PASS.

- [ ] **Step 5: Зафиксировать коммит**

```bash
cargo fmt --check
cargo clippy --all-targets --no-deps -- -D warnings
git add src/agent/run.rs src/reconcile.rs src/tests/reconcile.rs src/agent/tests/run.rs
git commit -m "fix(agent,reconcile): run prove_pending_metadata under flock with real minor and clean temp files"
```

---

### Task 5: Оптимизация запросов к Kubernetes API: кэширование PVC при проверке подов, фильтр watch-событий и устранение дублирующего `get` в `replica_cmd` (`H-4`, `L-6`)

**Files:**
- Modify: `src/agent/kube.rs:360-398, 796-912`
- Modify: `src/agent/replica_cmd.rs:115-145`
- Test: `src/agent/tests/kube.rs`, `src/agent/tests/replica_cmd.rs`

**Interfaces:**
- Consumes: `KubeVolumes`, `Volumes`
- Produces:
  - Per-call кэш `HashMap<(String, String), Option<String>>` в `pod_uses_volume_resolve` для предотвращения повторных `GET PVC` одного и того же claim в рамках обхода подов узла;
  - Быстрый пропуск подов без `persistent_volume_claim` и без CSI-томов до любых сетевых вызовов;
  - Параллельный `tokio::join!(volumes.list_fresh(), nodes.load())` в `run_replica` без лишнего `volumes.get(volume_name)`.

- [ ] **Step 1: Написать тест на работу `run_replica` и `pod_uses_volume_resolve` с кэшем PVC**

В `src/agent/tests/replica_cmd.rs` проверить, что за один шаг `run_replica` не делается лишний `get` поверх `list_fresh`, и все существующие тесты `replica_cmd` и `kube` проходят.

- [ ] **Step 2: Внести оптимизации в `src/agent/kube.rs` и `src/agent/replica_cmd.rs`**

1. В `src/agent/kube.rs`:
   - В `pod_uses_volume_resolve` добавить параметр `pvc_cache: &mut HashMap<(String, String), Option<String>>`.
   - Перед отправкой `pvcs.get_opt(&pvc_ref.claim_name)` проверять `pvc_cache.get(&(ns.to_string(), pvc_ref.claim_name.clone()))`. Если запись уже есть в кэше — использовать её без HTTP-запроса к K8s API; иначе делать запрос и сохранять `pvc.and_then(|p| p.spec).and_then(|s| s.volume_name)` в `pvc_cache`.
   - Создавать `let mut pvc_cache = HashMap::new();` в `delete_node_pods_for_volume` и `has_node_pods_for_volume`.
2. В `src/agent/replica_cmd.rs:L119-L140`:
   - Заменить три последовательных вызова (`volumes.get`, `volumes.list_fresh`, `nodes.load`) на:
     ```rust
     let (fresh_res, nodes_res) = tokio::join!(volumes.list_fresh(), nodes.load());
     let (all_volumes, cluster_nodes) = match (fresh_res, nodes_res) {
         (Ok(f), Ok(n)) => {
             consecutive_api_errors = 0;
             (f, n)
         }
         (Err(e), _) | (_, Err(e)) => {
             consecutive_api_errors += 1;
             if consecutive_api_errors >= 3 {
                 return Err(ReplicaError::Api(e));
             }
             sleep_step(deadline, &shutdown).await?;
             continue;
         }
     };
     let Some(volume) = all_volumes
         .iter()
         .find(|v| v.metadata.name.as_deref() == Some(volume_name))
         .cloned()
     else {
         return Err(ReplicaError::NotFound(volume_name.to_string()));
     };
     ```

- [ ] **Step 3: Запустить тесты `kube` и `replica_cmd`**

Run: `cargo test --lib agent::tests::kube agent::tests::replica_cmd && rm -rf target/debug/deps/ambar-*`
Expected: PASS.

- [ ] **Step 4: Зафиксировать коммит**

```bash
cargo fmt --check
cargo clippy --all-targets --no-deps -- -D warnings
git add src/agent/kube.rs src/agent/replica_cmd.rs
git commit -m "perf(kube,replica): cache PVC lookups per node scan and remove redundant get in run_replica"
```

---

### Task 6: Попировый учёт в `StandaloneTracker`, улучшение `Existing::max_peers` и `replicas::next_move` / проверка уникальности `node_id` и `address` (`H-3`, `M-2`, `M-3`)

**Files:**
- Modify: `src/agent/standalone.rs:27-111`
- Modify: `src/placement.rs:33-55, 177-187`
- Modify: `src/replicas.rs:255-280, 330-360, 575-635`
- Test: `src/agent/tests/standalone.rs`, `src/tests/placement.rs`, `src/tests/replicas.rs`

**Interfaces:**
- Produces:
  - `Existing { pub minor: u32, pub port: u16, pub size_bytes: u64, pub max_peers: u32, pub replica_nodes: Vec<String> }`
  - `StandaloneTracker`: сброс счётчика `attempts_made`, когда все пиры, которые ранее наблюдались в `StandAlone`, перешли в `Connected` (даже если третий пир находится в `Connecting`).
  - `ChangeError::NodeIdTaken { node: String, node_id: u32, holder: String }` и `ChangeError::AddressTaken { node: String, address: String, holder: String }` в `src/replicas.rs` (или переиспользование проверки перед добавлением нового узла) + `check_r25(view, cluster, Some(from))` на 2-м шаге `next_move`.

- [ ] **Step 1: Написать тесты для `StandaloneTracker`, `Existing::max_peers` и `replicas::next_move`**

1. В `src/agent/tests/standalone.rs` добавить тест:
```rust
#[test]
fn standalone_resets_when_standalone_peer_reconnects_even_if_third_peer_is_connecting() {
    let mut tracker = StandaloneTracker::new(3, Duration::from_secs(30));
    let t0 = Instant::now();

    let mut res = resource(
        Role::Secondary,
        vec![
            ("k3s-1", ConnectionState::StandAlone),
            ("k3s-3", ConnectionState::Connecting),
        ],
    );
    assert_eq!(tracker.check(&res, t0), StandaloneAction::RunAdjust { attempt: 1 });

    // k3s-1 успешно переподключился после adjust, а k3s-3 всё ещё выключен (Connecting)
    res.connections[0].connection_state = ConnectionState::Connected;
    assert_eq!(tracker.check(&res, t0 + Duration::from_secs(2)), StandaloneAction::None);

    // Если позже k3s-1 снова упадёт в StandAlone, отсчёт попыток должен начаться с 1, а не с 2
    res.connections[0].connection_state = ConnectionState::StandAlone;
    assert_eq!(
        tracker.check(&res, t0 + Duration::from_secs(40)),
        StandaloneAction::RunAdjust { attempt: 1 }
    );
}
```
2. В `src/tests/replicas.rs` добавить тест на 2-й шаг `next_move` (когда целевой узел `to` уже `UpToDate`, а старый узел `from` потерял статус `UpToDate` или `Connected`: `next_move` всё равно переводит `from` в `Retiring`, так как `check_r25` вызывается с `Some(from)`), а также тест на отказ при коллизии `node_id` или `address` в `next_add`.

- [ ] **Step 2: Запустить новые тесты и убедиться, что они падают**

Run: `cargo test --lib agent::tests::standalone::standalone_resets_when_standalone_peer_reconnects_even_if_third_peer_is_connecting`
Expected: FAIL (`left: RunAdjust { attempt: 2 }, right: RunAdjust { attempt: 1 }`).

- [ ] **Step 3: Реализовать изменения в `src/agent/standalone.rs`, `src/placement.rs` и `src/replicas.rs`**

1. В `src/agent/standalone.rs`:
   - Добавить в `VolumeState` поле `standalone_peers: HashSet<String>`.
   - Когда `has_standalone == true`, записывать имена всех пиров с `c.connection_state == ConnectionState::StandAlone` в `state.standalone_peers`.
   - Когда `has_standalone == false`, сбрасывать состояние (`self.states.remove(&res.name)`), если для каждого пира из `state.standalone_peers` его текущее соединение в `res.connections` находится в `ConnectionState::Connected` (либо все соединения `res.connections` в `Connected`).
2. В `src/placement.rs`:
   - Добавить поле `pub max_peers: u32` в `Existing` (заполнять `max_peers: object.spec.max_peers` в `existing_of`, а в тестовом хелпере `volume(...)` в `src/tests/placement.rs` и `src/tests/replicas.rs` выставлять `max_peers: DEFAULT_MAX_PEERS`).
   - В `free_budgets_aligned` использовать `v.max_peers` (или `v.max_peers.max(default_max_peers)`).
3. В `src/replicas.rs`:
   - На 2-м шаге `next_move` (`src/replicas.rs:L630`) заменить `check_r25(view, cluster, None)?;` на `check_r25(view, cluster, Some(from))?;`.
   - Добавить функцию `check_member_collision(nodes: &[MemberNode], name: &str, cnode: &ClusterNode) -> Result<(), ChangeError>` (проверяющую `n.name != name && (n.node_id == cnode.node_id || same_address(&n.address, cnode.address))`) и вызывать её перед добавлением нового узла в `next_add` и `next_move`, а в `find_witness` отфильтровывать кандидатов, чей `node_id` или `address` конфликтует с существующими участниками тома.

- [ ] **Step 4: Запустить тесты `standalone`, `placement` и `replicas`**

Run: `cargo test --lib agent::tests::standalone placement replicas && rm -rf target/debug/deps/ambar-*`
Expected: PASS.

- [ ] **Step 5: Зафиксировать коммит**

```bash
cargo fmt --check
cargo clippy --all-targets --no-deps -- -D warnings
git add src/agent/standalone.rs src/agent/tests/standalone.rs src/placement.rs src/tests/placement.rs src/replicas.rs src/tests/replicas.rs
git commit -m "fix(standalone,replicas): reset standalone per affected peer and allow retiring unhealthy source in move"
```

---

### Task 7: Устранение лишних аллокаций, точность метрик, проверка `read_only` при маунте, `[profile.release]` и `/readyz` (`M-5`, `M-6`, `L-1..5`)

**Files:**
- Modify: `src/drbd/status.rs:54-81`
- Modify: `src/agent/status.rs:73-84`
- Modify: `src/agent/run.rs:1815-1825, 1970-1988`
- Modify: `src/metrics.rs:103-117`
- Modify: `src/mount.rs:124-144`
- Modify: `src/csi/node.rs:434-455, 1018-1032`
- Modify: `src/agent/metrics_server.rs:76-120`
- Modify: `Cargo.toml`
- Modify: `image/Dockerfile:48-56`
- Test: `src/drbd/tests/status.rs`, `src/tests/metrics.rs`, `src/agent/tests/metrics_server.rs`

**Interfaces:**
- Produces:
  - `Role::as_str(&self) -> &'static str`, `DiskState::as_str(&self) -> &'static str`, `ConnectionState::as_str(&self) -> &'static str` (без `serde_json::to_string`).
  - Фильтрация `!d.client` при подсчёте `ambar_node_allocated_bytes` и проверка `c.connection_state == ConnectionState::Connected && d.volume == 0` при подсчёте `ambar_volume_uptodate_replicas`.
  - Безаллокационный `mount::parse_line` и однократное чтение `mountinfo` в `teardown_after_failed_stage`.
  - Секция `[profile.release]` в `Cargo.toml` и ассерт `grep -q` на `udev_sync = 0` в `image/Dockerfile`.

- [ ] **Step 1: Написать тесты на метрики (`Client` не увеличивает `allocated_bytes`, неподключённый пир не считается `UpToDate`) и `as_str()`**

В `src/tests/metrics.rs` добавить тест:
```rust
#[test]
fn render_metrics_ignores_disconnected_peer_in_uptodate_count() {
    let mut status = sample_status("vol-1", Role::Primary, DiskState::UpToDate, true);
    status.connections = vec![Connection {
        peer_node_id: 2,
        name: "k3s-2".to_string(),
        connection_state: ConnectionState::Connecting,
        peer_devices: vec![PeerDevice {
            volume: 0,
            replication_state: "Off".to_string(),
            peer_disk_state: DiskState::UpToDate,
            peer_client: false,
            resync_suspended: None,
            out_of_sync: 0,
        }],
    }];
    let data = NodeMetricData {
        node: "k3s-1",
        drbd_version: Some("9.3.4"),
        allocated_bytes: 0,
        is_tainted: false,
        volumes: vec![VolumeMetricData {
            name: "vol-1",
            status: Some(&status),
            manual_check_needed: false,
        }],
    };
    let text = render_metrics(&data);
    assert!(
        text.contains("ambar_volume_uptodate_replicas{node=\"k3s-1\",volume=\"vol-1\"} 1\n"),
        "disconnected peer must not count as uptodate: {text}"
    );
}
```

- [ ] **Step 2: Запустить тест и убедиться, что он падает**

Run: `cargo test --lib tests::metrics::render_metrics_ignores_disconnected_peer_in_uptodate_count`
Expected: FAIL (возвращает `2` вместо `1`).

- [ ] **Step 3: Реализовать изменения**

1. В `src/drbd/status.rs:L54-L80`:
   - Добавить `pub const fn as_str(&self) -> &'static str` для `Role`, `DiskState` и `ConnectionState` через прямой `match` и реализовать `fmt::Display` через `f.write_str(self.as_str())`.
   - В `src/agent/status.rs:L81` заменить `n.disk == DiskState::UpToDate.to_string()` на `n.disk == DiskState::UpToDate.as_str()`.
2. В `src/metrics.rs:L108-L113`:
   ```rust
   let peers = status
       .connections
       .iter()
       .filter(|c| {
           c.connection_state == ConnectionState::Connected
               && c.peer_devices
                   .iter()
                   .any(|d| d.volume == 0 && d.peer_disk_state == DiskState::UpToDate)
       })
       .count();
   ```
3. В `src/agent/run.rs:L1819`:
   ```rust
   let size = status.device().filter(|d| !d.client).map_or(0, |d| d.size * 1024);
   ```
   И в `Queue::pop_due` (`src/agent/run.rs:L1977-L1986`) клонировать `name` только внутри ветки `if at <= now`.
4. В `src/mount.rs:L124-L143`:
   - Переписать `parse_line` на итератор `.split_whitespace()` без `.collect::<Vec<_>>()`:
     ```rust
     fn parse_line(line: &str) -> Option<MountEntry> {
         let (left, right) = line.split_once(" - ")?;
         let mut left_it = left.split_whitespace();
         let _id = left_it.next()?;
         let _parent = left_it.next()?;
         let _major_minor = left_it.next()?;
         let root = left_it.next()?;
         let target = left_it.next()?;
         let options = left_it.next()?;

         let mut right_it = right.split_whitespace();
         let fstype = right_it.next()?;
         let source = right_it.next()?;

         Some(MountEntry {
             root: unescape(root),
             target: PathBuf::from(unescape(target)),
             read_only: options.split(',').any(|option| option == "ro"),
             fstype: fstype.to_string(),
             source: unescape(source),
         })
     }
     ```
5. В `src/csi/node.rs:L1019-L1028` (`teardown_after_failed_stage`):
   - Прочитать и распарсить `m.mountinfo` один раз через `m.entries()?`, определив и `staged`, и `in_use` по одному списку `entries`.
6. В `Cargo.toml`:
   ```toml
   [profile.release]
   lto = "thin"
   codegen-units = 1
   strip = "symbols"
   ```
7. В `image/Dockerfile:L52-L53`:
   - Добавить проверку после `sed`:
     ```dockerfile
     && grep -q '^[[:space:]]*udev_sync = 0' /etc/lvm/lvm.conf \
     && grep -q '^[[:space:]]*udev_rules = 0' /etc/lvm/lvm.conf \
     ```

- [ ] **Step 4: Запустить полный набор тестов, линтер и обновить индекс кода**

Run:
```bash
python3 scripts/code-index.py
cargo fmt --check
cargo clippy --all-targets --no-deps -- -D warnings
cargo test
cargo clean
```
Expected: Все тесты (`lib` и `bin`), `clippy` и `fmt` проходят без единого предупреждения, каталог `target/` очищен.

- [ ] **Step 5: Зафиксировать финальный коммит**

```bash
git add Cargo.toml image/Dockerfile src/drbd/status.rs src/agent/status.rs src/agent/run.rs src/metrics.rs src/tests/metrics.rs src/mount.rs src/csi/node.rs
git commit -m "perf,fix: remove Display serde allocations, refine metrics, optimize mountinfo and add release profile"
```
