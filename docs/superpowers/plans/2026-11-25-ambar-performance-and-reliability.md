# Производительность и надёжность агента ambar — План реализации (ревизия 2)

> **Для агентов-исполнителей:** ОБЯЗАТЕЛЬНЫЙ SUB-SKILL: используйте `superpowers:subagent-driven-development` (рекомендуется) или `superpowers:executing-plans` для пошаговой реализации задачи за задачей. Прогресс отмечается чекбоксами (`- [x]`).

**Цель:** Убрать паразитную фоновую CPU/IO/API-нагрузку агента, закрыть кросс-процессную гонку `mkfs.ext4`, устранить head-of-line blocking в `serve` и зомби-сироты при PID 1, сохранив все инварианты безопасности данных и fail-closed поведение.

**Архитектура:** Дебаунс и переиспользование снимков состояния в `tick_failures` (обратный индекс маунтов в `spawn_blocking`, кэш версии DRBD, `CleanupTracker` 30 с, очистка трекеров); передача снимка объектов в рамках полного прохода `serve`; неблокирующий `VolumeLocks::try_lock` в `serve`; файловый `flock` + повторная проверка сигнатуры вокруг `mkfs.ext4`; ограничение памяти `exec::drain` с дочитыванием в `io::sink()`; инициализатор `tini` как PID 1 в образе и `DaemonSet`.

**Стек:** Rust (Edition 2024), `kube` 5.1, `tokio`, `tonic`, DRBD 9.3.4, LVM2, Debian 13 trixie (`tini`).

**Спецификация:** `docs/superpowers/specs/2026-11-25-ambar-performance-and-reliability.md`

## Глобальные ограничения

1. **Строгий TDD:** сначала падающий тест → проверка падения → минимальная реализация → зелёный прогон.
2. **Конвенция тестов:** тесты модуля `src/<dir>/foo.rs` живут строго в `src/<dir>/tests/foo.rs` (для корня `src/foo.rs` — в `src/tests/foo.rs`). Не добавлять inline `#[cfg(test)] mod tests { ... }` и не создавать корневой каталог `tests/`.
3. **Сохранность тестов:** все существующие тесты обязаны проходить без ослабления проверок.
4. **Неприкосновенные инварианты:**
   - таблица безопасности данных `src/agent/run.rs:7-18` (`remove_checked` / `remove_unconfigured` под `flock`);
   - классификация метаданных DRBD `src/drbd/adm.rs:93-293` и запрет `create-md` при сконфигурированном ресурсе `src/reconcile.rs:228-238`;
   - инвариант `src/exec.rs:289-315` (`waitid(WNOWAIT)` — не делать reap лидера до `kill_group`);
   - fail-closed в `tick_failures` (`ceb7ba8` — сбой `read_all_statuses` прерывает тик без эвикции по устаревшим данным);
   - семантика DRBD `max_peers` (`spec.nodes.len() <= max_peers + 1`, `src/membership.rs:60` НЕ менять).
5. **Навигация и гигиена:** не читать `target/`, `.git/`, `.idea/`, `.superpowers/`, `Cargo.lock` целиком; для структурного поиска использовать `ast-grep` и `.code-index/repo-map.md`. После структурных изменений в `src/` обновлять индекс `python3 scripts/code-index.py`.
6. **Двуязычная документация:** любые правки в `README.md` синхронно вносить в `README.ru.md`.
7. **Проверки после каждой задачи и очистка диска:**
   - `cargo fmt --check`
   - `cargo clippy --all-targets --no-deps -- -D warnings`
   - `cargo test`
   - очистка старых тестовых бинарников в `target/debug/deps/` по правилам Rust-проектов, в финале после пуша — `cargo clean`.

---

### Задача 1: Гигиена рантайма, zero-copy `MetricsHolder` и дедупликация бюджетов (`Н7`, `Н12`, `Н13`, `В11`)

**Файлы:**
- Модифицировать: `src/metrics.rs:36-59`
- Модифицировать: `src/agent/metrics_server.rs:30-95`
- Модифицировать: `src/main.rs:172-225`
- Модифицировать: `src/placement.rs:144-195`
- Модифицировать: `README.md:38-45`
- Модифицировать: `README.ru.md:38-45`
- Тесты: `src/tests/metrics.rs`, `src/tests/placement.rs`

**Интерфейсы:**
- Производит:
  - `MetricsHolder::get(&self) -> Arc<str>` (вместо `String`, с восстановлением после poisoned `RwLock`).
  - `metrics_server::serve_listener(listener: TcpListener, provider: Arc<dyn Fn() -> Arc<str> + Send + Sync>, shutdown: CancellationToken) -> io::Result<()>`.
  - Внутренний хелпер `budgets_by<'a>(nodes: &'a ClusterNodes, existing: &[Existing], size_of: impl Fn(&Existing) -> u64) -> HashMap<&'a str, u64>` в `src/placement.rs`.

- [x] **Шаг 1: Написать падающий тест на `Arc<str>` и устойчивость `MetricsHolder` к poisoned lock в `src/tests/metrics.rs`**

```rust
#[test]
fn metrics_holder_shares_arc_and_recovers_from_poisoned_lock() {
    let holder = MetricsHolder::new();
    holder.update("ambar_node_tainted{node=\"n1\"} 0\n".to_string());
    let first = holder.get();
    let second = holder.get();
    assert!(std::sync::Arc::ptr_eq(&first, &second));

    let poisoned = holder.clone();
    let _ = std::panic::catch_unwind(move || {
        let _guard = poisoned.write_guard_for_test();
        panic!("boom");
    });
    assert_eq!(&*holder.get(), "ambar_node_tainted{node=\"n1\"} 0\n");
    holder.update("updated\n".to_string());
    assert_eq!(&*holder.get(), "updated\n");
}
```

- [x] **Шаг 2: Убедиться, что тест не компилируется / падает**

Выполнить: `cargo test metrics_holder_shares_arc`  
Ожидаемо: ошибка компиляции (`ptr_eq` на `String`, отсутствует `Arc<str>`).

- [x] **Шаг 3: Реализовать `Arc<str>` в `src/metrics.rs` и `src/agent/metrics_server.rs`, перенести `log_allocator_page_size` в `src/main.rs`, дедуплицировать `free_budgets` в `src/placement.rs`**

В `src/metrics.rs`:
```rust
#[derive(Clone)]
pub struct MetricsHolder {
    rendered: std::sync::Arc<std::sync::RwLock<std::sync::Arc<str>>>,
}

impl Default for MetricsHolder {
    fn default() -> Self {
        Self::new()
    }
}

impl MetricsHolder {
    pub fn new() -> Self {
        Self {
            rendered: std::sync::Arc::new(std::sync::RwLock::new(std::sync::Arc::from(""))),
        }
    }

    pub fn update(&self, text: String) {
        let mut lock = self.rendered.write().unwrap_or_else(|e| e.into_inner());
        *lock = std::sync::Arc::from(text);
    }

    pub fn get(&self) -> std::sync::Arc<str> {
        self.rendered.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    #[cfg(test)]
    pub(crate) fn write_guard_for_test(&self) -> std::sync::RwLockWriteGuard<'_, std::sync::Arc<str>> {
        self.rendered.write().unwrap_or_else(|e| e.into_inner())
    }
}
```

В `src/agent/metrics_server.rs` заменить тип `provider` в `serve_listener` и `handle_connection` на `Arc<dyn Fn() -> Arc<str> + Send + Sync>`, а в местах вызова в `src/agent/tests/metrics_server.rs` обернуть строку в `Arc::from(...)`.

В `src/main.rs` перенести вызов:
```rust
    #[cfg(all(unix, feature = "jemalloc"))]
    log_allocator_page_size();
```
в самое начало `run_agent` (перед чтением `hostname` и `preflight`).

В `src/placement.rs`:
```rust
/// Standard LVM Physical Extent (PE) size: 4 MiB (`vgcreate -s 4m`, LVM2 default).
/// Node capacity accounting assumes the backing Volume Group uses 4 MiB extents.
pub const LVM_PE_SIZE: u64 = 4 * 1024 * 1024;

fn budgets_by<'a>(
    nodes: &'a ClusterNodes,
    existing: &[Existing],
    size_of: impl Fn(&Existing) -> u64,
) -> HashMap<&'a str, u64> {
    let mut used: HashMap<&str, u64> = HashMap::new();
    for volume in existing {
        let bytes = size_of(volume);
        for name in &volume.replica_nodes {
            let slot = used.entry(name.as_str()).or_insert(0);
            *slot = slot.saturating_add(bytes);
        }
    }
    nodes
        .iter()
        .map(|n| {
            let taken = used.get(n.name.as_str()).copied().unwrap_or(0);
            (n.name.as_str(), n.capacity_bytes.saturating_sub(taken))
        })
        .collect()
}

pub fn free_budgets_aligned<'a>(
    nodes: &'a ClusterNodes,
    existing: &[Existing],
    default_max_peers: u32,
) -> HashMap<&'a str, u64> {
    budgets_by(nodes, existing, |v| lvm_allocated_bytes(v.size_bytes, default_max_peers))
}

pub fn free_budgets<'a>(nodes: &'a ClusterNodes, existing: &[Existing]) -> HashMap<&'a str, u64> {
    budgets_by(nodes, existing, |v| v.size_bytes)
}
```

В `README.md` и `README.ru.md` в пункте про LVM (`--volume-group=k3s-vg`) явно указать требование стандартного размера экстента LVM (`PE = 4 MiB`, дефолт `vgcreate`).

- [x] **Шаг 4: Прогнать форматтер, линтер и тесты**

Выполнить:
```bash
cargo fmt --check
cargo clippy --all-targets --no-deps -- -D warnings
cargo test
```
Ожидаемо: PASS.

---

### Задача 2: Защита `exec::drain` от OOM без `SIGPIPE` и отдельный таймаут `blkdiscard` (`В3`, `Н8`)

**Файлы:**
- Модифицировать: `src/exec.rs:323-339`
- Модифицировать: `src/backing.rs:362-395, 407-426`
- Тесты: `src/tests/exec.rs`, `src/tests/backing.rs`

**Интерфейсы:**
- Производит:
  - `pub const MAX_OUTPUT_CAPTURE: usize = 1024 * 1024;` в `src/exec.rs`.
  - `pub const BLKDISCARD_TIMEOUT: Duration = Duration::from_secs(15);` в `src/backing.rs`.

- [x] **Шаг 1: Написать падающие тесты в `src/tests/exec.rs` и `src/tests/backing.rs`**

В `src/tests/exec.rs`:
```rust
#[test]
fn drain_caps_captured_stdout_without_sigpipe() {
    let runner = SystemRunner::default();
    // Writes 2 MiB to stdout and exits 0 only if write succeeded without SIGPIPE.
    let script = "dd if=/dev/zero bs=65536 count=32 2>/dev/null | tr '\\0' 'a'";
    let out = runner.run("/bin/sh", &["-c", script]).expect("command must finish");
    assert_eq!(out.code, 0, "writer must not die with SIGPIPE");
    assert!(out.stdout.len() <= MAX_OUTPUT_CAPTURE + 64);
    assert!(out.stdout.ends_with("\n... [output truncated]"));
}
```

В `src/tests/backing.rs`: проверить через кастомный `Runner`, записывающий переданный `timeout` в `run_with_timeout`, что для `blkdiscard` в `create_lvm_volume` и `remove_lvm_volume` передаётся `BLKDISCARD_TIMEOUT` (`15s`), тогда как `lvcreate` и `lvremove` идут через штатный `run`.

- [x] **Шаг 2: Запустить тесты и убедиться в падении**

Выполнить: `cargo test drain_caps_captured_stdout_without_sigpipe`  
Ожидаемо: ошибка компиляции (`MAX_OUTPUT_CAPTURE` не определён).

- [x] **Шаг 3: Реализовать лимитированный `drain` и `checked_with_timeout` для `blkdiscard`**

В `src/exec.rs`:
```rust
/// Maximum bytes kept in memory per stream (`stdout` / `stderr`). Any further
/// output is drained to `io::sink()` so the child never hits `SIGPIPE`.
pub const MAX_OUTPUT_CAPTURE: usize = 1024 * 1024;
const TRUNCATED_SUFFIX: &[u8] = b"\n... [output truncated]";

fn drain<R: Read + Send + 'static>(
    stream: Stream,
    pipe: Option<R>,
    sender: mpsc::Sender<(Stream, io::Result<Vec<u8>>)>,
) {
    thread::spawn(move || {
        let result = match pipe {
            Some(mut pipe) => read_capped(&mut pipe),
            None => Ok(Vec::new()),
        };
        let _ = sender.send((stream, result));
    });
}

fn read_capped<R: Read>(pipe: &mut R) -> io::Result<Vec<u8>> {
    let limit = MAX_OUTPUT_CAPTURE as u64;
    let mut bytes = Vec::new();
    pipe.by_ref().take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() > MAX_OUTPUT_CAPTURE {
        bytes.truncate(MAX_OUTPUT_CAPTURE);
        bytes.extend_from_slice(TRUNCATED_SUFFIX);
        let _ = io::copy(pipe, &mut io::sink())?;
    }
    Ok(bytes)
}
```

В `src/backing.rs`:
```rust
/// Time limit for best-effort `blkdiscard` so a slow controller does not stall LV operations for 120 s.
pub const BLKDISCARD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
```
Добавить `checked_with_timeout(runner, program, args, timeout)` через `runner.run_with_timeout(...)` и заменить оба вызова `checked(runner, "blkdiscard", ...)` в `create_lvm_volume` и `remove_lvm_volume` на `checked_with_timeout(..., BLKDISCARD_TIMEOUT)`.

- [x] **Шаг 4: Прогнать проверки**

Выполнить:
```bash
cargo fmt --check
cargo clippy --all-targets --no-deps -- -D warnings
cargo test
```
Ожидаемо: PASS.

---

### Задача 3: Экспоненциальный backoff в ретраях K8s-конфликтов (`Н4`)

**Файлы:**
- Модифицировать: `src/agent/kube.rs:227-258, 706-758`
- Модифицировать: `src/agent/cleanup.rs:100-126`
- Модифицировать: `src/csi/node.rs:645-685, 821-855`
- Тесты: `src/agent/tests/kube.rs`

**Интерфейсы:**
- Производит: `pub(crate) fn conflict_backoff(attempt: usize) -> Duration` в `src/agent/kube.rs` (`20ms * (1 << attempt.min(4))`).

- [x] **Шаг 1: Написать падающий тест в `src/agent/tests/kube.rs`**

Проверить с `tokio::time::pause()`, что при серии `ApiError::Conflict` в `patch_retrying` виртуальное время продвигается на сумму задержек `20ms + 40ms + ...`, а при успехе с первой попытки задержка равна `0ms`.

- [x] **Шаг 2: Запустить тест и убедиться в падении**

Выполнить: `cargo test patch_retrying_backs_off_on_conflict`  
Ожидаемо: FAIL (время `elapsed` равно `0ms`).

- [x] **Шаг 3: Внедрить `conflict_backoff` между попытками**

В `src/agent/kube.rs`:
```rust
pub(crate) fn conflict_backoff(attempt: usize) -> Duration {
    Duration::from_millis(20 * (1u64 << attempt.min(4)))
}
```
В `edit_retrying` (`src/agent/kube.rs`), `ensure_node_taint` / `remove_node_taint` (`src/agent/kube.rs`), `leave_client` (`src/agent/cleanup.rs`), `join` и `leave` (`src/csi/node.rs`): при конфликте, если `attempt + 1 < EDIT_ATTEMPTS`, вызывать `tokio::time::sleep(conflict_backoff(attempt)).await`.

- [x] **Шаг 4: Прогнать проверки**

Выполнить:
```bash
cargo fmt --check
cargo clippy --all-targets --no-deps -- -D warnings
cargo test
```
Ожидаемо: PASS.

---

### Задача 4: Разгрузка `tick_failures`, дебаунс cleanup и очистка трекеров (`К1`, `В1`, `В2`, `В4`, `В8`)

**Файлы:**
- Модифицировать: `src/agent/kube.rs:389-407`
- Модифицировать: `src/agent/quorum.rs:66-76`
- Модифицировать: `src/agent/standalone.rs:100-107`
- Модифицировать: `src/agent/cleanup.rs:1-97`
- Модифицировать: `src/agent/run.rs:1600-1757`
- Тесты: `src/agent/tests/kube.rs`, `src/agent/tests/quorum.rs`, `src/agent/tests/standalone.rs`, `src/agent/tests/cleanup.rs`, `src/agent/tests/run.rs`

**Интерфейсы:**
- Производит:
  - `pub fn mounted_pod_uids_by_volume(kubelet_pods_dir: &Path) -> HashMap<String, Vec<String>>` в `src/agent/kube.rs`.
  - `QuorumTracker::retain_volumes(&mut self, active: &HashSet<&str>)` в `src/agent/quorum.rs`.
  - `StandaloneTracker::retain_volumes(&mut self, active: &HashSet<&str>)` в `src/agent/standalone.rs`.
  - `CleanupTracker` (`pub const DEFAULT_CLEANUP_INTERVAL: Duration = Duration::from_secs(30);`, `CleanupTracker::new(interval: Duration)`, `CleanupTracker::should_run(&mut self, now: Instant) -> bool`) в `src/agent/cleanup.rs`.
  - `check_and_cleanup_client_with_snapshot(agent: &Agent, volume: &str, object: &ReplicatedVolume, snapshot: Option<&ResourceStatus>, mounted_uids: &[String]) -> Result<Option<Outcome>, AgentError>` в `src/agent/cleanup.rs` (существующий `check_and_cleanup_client` оставить тонкой обёрткой для обратной совместимости тестов).

- [x] **Шаг 1: Написать падающие тесты**

1. В `src/agent/tests/kube.rs`: `mounted_pod_uids_by_volume_indexes_multiple_volumes_in_one_scan` — создаёт во временном каталоге два пода с разными CSI-томами (и невалидным именем каталога) и проверяет точный `HashMap<String, Vec<String>>`.
2. В `src/agent/tests/quorum.rs` и `src/agent/tests/standalone.rs`: `retain_volumes_drops_vanished_entries` — проверяет удаление исчезнувших томов и сохранение активных.
3. В `src/agent/tests/cleanup.rs`:
   - `cleanup_tracker_debounces_within_interval` — проверяет `should_run(t0) == true`, `should_run(t0 + 5s) == false`, `should_run(t0 + 30s) == true`;
   - `snapshot_busy_or_mounted_skips_status_and_pod_list` — проверяет, что если в `snapshot` ресурс `open: true` или `mounted_uids` непуст, `has_node_pods_for_volume` и `drbdsetup` вообще не вызываются.
4. В `src/agent/tests/run.rs`:
   - `tick_failures_forgets_vanished_volumes_and_stays_fail_closed_on_drbd_error` — проверяет, что при исчезновении тома из `statuses` трекер очищается, но при ошибке `drbdsetup` трекер и `is_tainted` сохраняют прежнее состояние (fail-closed).

- [x] **Шаг 2: Запустить новые тесты и убедиться в падении**

Выполнить: `cargo test mounted_pod_uids_by_volume`  
Ожидаемо: ошибка компиляции (новые функции ещё не созданы).

- [x] **Шаг 3: Реализовать индекс маунтов, `CleanupTracker`, `retain_volumes`, кэш версии DRBD и обновлённый `tick_failures`**

1. В `src/agent/kube.rs`:
```rust
pub fn mounted_pod_uids_by_volume(
    kubelet_pods_dir: &std::path::Path,
) -> std::collections::HashMap<String, Vec<String>> {
    let mut index: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
    let Ok(pods) = std::fs::read_dir(kubelet_pods_dir) else {
        return index;
    };
    for pod_entry in pods.flatten() {
        let pod_uid = pod_entry.file_name().to_string_lossy().into_owned();
        let csi_dir = pod_entry.path().join("volumes/kubernetes.io~csi");
        let Ok(vols) = std::fs::read_dir(csi_dir) else {
            continue;
        };
        for vol_entry in vols.flatten() {
            let volume = vol_entry.file_name().to_string_lossy().into_owned();
            if crate::model::valid_volume_name(&volume) {
                index.entry(volume).or_default().push(pod_uid.clone());
            }
        }
    }
    index
}
```
2. В `src/agent/quorum.rs` и `src/agent/standalone.rs`:
```rust
pub fn retain_volumes(&mut self, active: &std::collections::HashSet<&str>) {
    self.lost_at.retain(|name, _| active.contains(name.as_str())); // states.retain в StandaloneTracker
}
```
3. В `src/agent/cleanup.rs`: добавить `CleanupTracker` и `check_and_cleanup_client_with_snapshot`, который до взятия `agent.locks.lock(volume)` проверяет `is_client`, отсутствие `Primary`/`open` в `snapshot` и `mounted_uids.is_empty()`. Только если все три условия выполнены — берёт лок, делает контрольный `agent.read_status(volume)` и вызывает `has_node_pods_for_volume`.
4. В `src/agent/run.rs`:
   - Кэшировать `/sys/module/drbd/version` через `static DRBD_VERSION: OnceLock<Option<String>> = OnceLock::new();` (инициализируя внутри `spawn_blocking`).
   - Вычислять `mounted_by_volume` один раз за тик через `tokio::task::spawn_blocking(|| mounted_pod_uids_by_volume(Path::new("/var/lib/kubelet/pods"))).await`.
   - После обхода `statuses` вызывать `retain_volumes(&active)` на `quorum_tracker` и `standalone_tracker`.
   - Запускать блок 4 (`Stale Client cleanup`) через `CleanupTracker` (раз в 30 с), передавая статус из карты `statuses` и `mounted_uids` из `mounted_by_volume` (в существующую сигнатуру `tick_failures` оставить обёртку или добавить `cleanup_tracker: &mut CleanupTracker`, обновив вызовы в `run_failure_loop` и `src/agent/tests/run.rs`).
   - В `run_failure_loop` установить интервал `Duration::from_secs(2)`.

- [x] **Шаг 4: Прогнать проверки и обновить AST-индекс**

Выполнить:
```bash
python3 scripts/code-index.py
cargo fmt --check
cargo clippy --all-targets --no-deps -- -D warnings
cargo test
```
Ожидаемо: PASS.

---

### Задача 5: Снимок коллизий за полный проход `serve` (`К5`)

**Файлы:**
- Модифицировать: `src/agent/run.rs:788-825, 1519-1598`
- Тесты: `src/agent/tests/run.rs`

**Интерфейсы:**
- Производит:
  - Внутренний метод `reconcile_one_with_peers(&self, volume: &str, peers_snapshot: Option<&[(String, u32, u16)]>) -> Result<Outcome, AgentError>` (публичный `reconcile_one(&self, volume: &str)` вызывает `self.reconcile_one_with_peers(volume, None).await`).

- [x] **Шаг 1: Написать падающий тест в `src/agent/tests/run.rs`**

Создать фейковый `Volumes` со счётчиком вызовов `list_calls: AtomicUsize` и 4 объектами `ReplicatedVolume`. Запустить `agent.run(shutdown)` до выставления `agent.ready == true` и остановить `shutdown.cancel()`. Проверить `assert_eq!(volumes.list_calls(), 1)` (вместо `1 + 4 = 5` вызовов на старте).

- [x] **Шаг 2: Запустить тест и убедиться в падении**

Выполнить: `cargo test first_pass_lists_volumes_once`  
Ожидаемо: FAIL (`assertion left == right failed: left: 5, right: 1`).

- [x] **Шаг 3: Реализовать передачу снимка `(name, minor, port)` в полном проходе**

В `src/agent/run.rs`:
- Изменить `all_names` так, чтобы возвращать `(BTreeSet<String>, Vec<(String, u32, u16)>)` из одного вызова `self.volumes.list().await?`.
- В `serve` сохранять `full_pass_peers: Option<(BTreeSet<String>, Vec<(String, u32, u16)>)>` для имён текущего полного прохода; при поступлении события из `watch` (добавить `watch_dirty: AtomicBool` в `Queue`, выставляемый в `feed`) сбрасывать снимок `full_pass_peers = None`, чтобы изменения из `watch` всегда видели актуальный список коллизий.

- [x] **Шаг 4: Прогнать проверки**

Выполнить:
```bash
cargo fmt --check
cargo clippy --all-targets --no-deps -- -D warnings
cargo test
```
Ожидаемо: PASS.

---

### Задача 6: Сужение `placing`-лока контроллера, семафор `NodeService` и `spawn_blocking` (`В5`, `В6`, `Н6`)

**Файлы:**
- Модифицировать: `src/csi/controller.rs:201-248`
- Модифицировать: `src/csi/node.rs:510-560, 775-786, 1018-1140`
- Тесты: `src/csi/tests/controller.rs`, `src/csi/tests/node.rs`

**Интерфейсы:**
- Производит:
  - `pub const MAX_CONCURRENT_NODE_OPS: usize = 16;` в `src/csi/node.rs`.
  - Конструктор `NodeService::with_concurrency_limit` (для быстрого тестирования с лимитом `2`).

- [x] **Шаг 1: Написать падающие тесты в `src/csi/tests/controller.rs` и `src/csi/tests/node.rs`**

1. В `src/csi/tests/controller.rs`: тест `create_volume_reads_node_map_outside_placing_lock` — захватывает `service.placing` вручную в тесте и проверяет через `tokio::time::timeout`, что параллельный вызов `find_or_create` успевает выполнить `nodes.load()` до ожидания `self.placing.lock()`.
2. В `src/csi/tests/node.rs`: тест `node_ops_respect_concurrency_semaphore` — создаёт сервис с лимитом `2`, блокирует 2 вызова `node_publish_volume` на барьере и проверяет, что 3-й вызов ждёт освобождения слота семафора, не порождая фоновую задачу раньше времени.

- [x] **Шаг 2: Запустить тесты и убедиться в падении**

Выполнить: `cargo test create_volume_reads_node_map_outside_placing_lock`  
Ожидаемо: FAIL (таймаут — `nodes.load()` заблокирован под `placing`).

- [x] **Шаг 3: Реализовать сужение `placing`-лока, семафор `NodeService` и `spawn_blocking` для `create_dir_all`**

1. В `src/csi/controller.rs` (`find_or_create`):
   - До `let _placing = self.placing.lock().await;` вызывать быстрый `if let Some(object) = self.volumes.get(name).await? { ... }` и `let nodes = self.nodes.load().await?;`.
   - Под `_placing` оставить повторный `self.volumes.get(name).await?`, `self.volumes.list_fresh().await?`, `place(&wanted, &nodes, &existing)?` и `self.volumes.create(&object).await?`.
2. В `src/csi/node.rs`:
   - Добавить `concurrency: Arc<tokio::sync::Semaphore>` в `NodeService` (`Semaphore::new(MAX_CONCURRENT_NODE_OPS)`).
   - В `detached` (или обёртке `self.run_detached(rpc, &volume, work)`) захватывать `let permit = self.concurrency.clone().acquire_owned().await.map_err(|_| Status::unavailable("shutting down"))?;` до `tokio::spawn` и перемещать `let _permit = permit;` внутрь spawned future.
   - В `stage_locked` обернуть `std::fs::create_dir_all(&staging)` в `tokio::task::spawn_blocking` аналогично `publish_locked`.

- [x] **Шаг 4: Прогнать проверки**

Выполнить:
```bash
cargo fmt --check
cargo clippy --all-targets --no-deps -- -D warnings
cargo test
```
Ожидаемо: PASS.

---

### Задача 7: Неблокирующий `VolumeLocks::try_lock` в `serve` против head-of-line blocking (`К3`)

**Файлы:**
- Модифицировать: `src/agent/run.rs:244-262, 788-805, 1556-1598`
- Тесты: `src/agent/tests/run.rs`

**Интерфейсы:**
- Производит:
  - `pub fn try_lock(&self, volume: &str) -> Option<OwnedMutexGuard<()>>` в `VolumeLocks` (`src/agent/run.rs`).
  - `const BUSY_VOLUME_RETRY_DELAY: Duration = Duration::from_secs(1);` в `src/agent/run.rs`.

- [x] **Шаг 1: Написать падающий тест в `src/agent/tests/run.rs`**

Тест `serve_skips_locked_volume_and_reconciles_others_without_marking_ready_early`:
- Создаёт два тома `vol-a` и `vol-b`.
- Заранее удерживает `let guard = agent.locks.lock("vol-a").await;` в тесте (имитируя долгий `mkfs` в CSI).
- Запускает `agent.run(shutdown)`:
  1. Проверяет, что `vol-b` успешно проходит реконсиляцию, не дожидаясь `vol-a`!
  2. Проверяет, что `agent.ready` остаётся `false`, пока `guard` удерживается (`startup` не засчитывает пропущенный `vol-a`).
  3. Отпускает `drop(guard)` — после `BUSY_VOLUME_RETRY_DELAY` `vol-a` проходит реконсиляцию и `agent.ready` становится `true`.

- [x] **Шаг 2: Запустить тест и убедиться в падении**

Выполнить: `cargo test serve_skips_locked_volume`  
Ожидаемо: FAIL (таймаут — `serve` завис на `vol-a` перед `vol-b`).

- [x] **Шаг 3: Реализовать `VolumeLocks::try_lock` и пропуск занятого тома в `serve`**

В `src/agent/run.rs`:
```rust
impl VolumeLocks {
    fn entry(&self, volume: &str) -> Arc<AsyncMutex<()>> {
        let mut locks = self.locks.lock().unwrap_or_else(|e| e.into_inner());
        locks.retain(|_, lock| Arc::strong_count(lock) > 1);
        locks.entry(volume.to_string()).or_default().clone()
    }

    pub async fn lock(&self, volume: &str) -> OwnedMutexGuard<()> {
        self.entry(volume).lock_owned().await
    }

    pub fn try_lock(&self, volume: &str) -> Option<OwnedMutexGuard<()>> {
        self.entry(volume).try_lock_owned().ok()
    }
}
```
В `serve`: вызывать внутренний `try_reconcile_one(&name, peers)`:
- если имя невалидно — штатно возвращать `Ok(Some(Outcome::Refused(...)))`;
- если `self.locks.try_lock(volume)` вернул `None` — возвращать `Ok(None)`;
- в `serve` при `Ok(None)` планировать `queue.schedule(&name, Instant::now() + BUSY_VOLUME_RETRY_DELAY)` и переходить к следующему элементу очереди **без** вызова `startup.record`.

- [x] **Шаг 4: Прогнать проверки**

Выполнить:
```bash
cargo fmt --check
cargo clippy --all-targets --no-deps -- -D warnings
cargo test
```
Ожидаемо: PASS.

---

### Задача 8: Кросс-процессный файловый `flock` и повторная сверка сигнатуры вокруг `mkfs.ext4` (`В7`)

**Файлы:**
- Модифицировать: `src/csi/node.rs:764-786`
- Тесты: `src/csi/tests/node.rs`

**Интерфейсы:**
- Использует: `Reconciler::new(paths, runner).lock_volume(&volume)` (`src/reconcile.rs:706`) и `Mount::new(runner, &mountinfo)` (`src/mount.rs`).

- [x] **Шаг 1: Написать два падающих теста в `src/csi/tests/node.rs`**

1. `stage_refuses_mkfs_with_unavailable_when_volume_flock_is_held`:
   - Том не отформатирован (`blkid` возвращает код 2, аннотации `formatted` нет).
   - Внешний дескриптор держит эксклюзивный `flock` на `paths.lock_file(volume)` (имитация старого пода при rolling restart).
   - Вызов `node_stage_volume` обязан вернуть `Code::Unavailable` и **не** вызывать `mkfs.ext4`.
2. `stage_skips_mkfs_when_peer_formatted_volume_before_flock_acquired`:
   - Первая проверка `signature` вернула `Unformatted`, но ко второй проверке внутри `flock`-секции `blkid` уже возвращает `TYPE="ext4"` (первый процесс завершил `mkfs` прямо перед нами, но ещё не успел записать аннотацию).
   - Вызов `node_stage_volume` **не** запускает `mkfs.ext4`, проставляет `mark_formatted` и монтирует том.

- [x] **Шаг 2: Запустить тесты и убедиться в падении**

Выполнить: `cargo test stage_refuses_mkfs_with_unavailable_when_volume_flock_is_held`  
Ожидаемо: FAIL (`mkfs.ext4` вызывается поверх чужого `flock`).

- [x] **Шаг 3: Реализовать защищённый `format_under_flock` в `src/csi/node.rs`**

В `src/csi/node.rs` в ветке `Format::FormatThenMark`:
```rust
            Format::FormatThenMark => {
                self.still_unformatted(&object, &volume).await?;
                self.format_under_flock(&volume, &device).await?;
                self.mark_formatted(&object, &volume).await?;
            }
```
Метод `format_under_flock(&self, volume: &str, device: &Path) -> Result<(), Status>`:
```rust
    async fn format_under_flock(&self, volume: &str, device: &Path) -> Result<(), Status> {
        let name = volume.to_string();
        let target = device.to_path_buf();
        let mountinfo = self.mountinfo.clone();
        let outcome = self
            .agent
            .blocking(move |paths, runner| {
                let _flock = Reconciler::new(paths, runner).lock_volume(&name)?;
                let mount = Mount::new(runner, &mountinfo);
                match mount.signature(&target) {
                    Ok(Signature::Unformatted) => mount.mkfs_ext4(&target).map_err(FormatLockError::Mount),
                    Ok(Signature::Ext4) => Ok(()),
                    Ok(Signature::Other(fs)) => Err(FormatLockError::ForeignFs(fs)),
                    Err(e) => Err(FormatLockError::Mount(e)),
                }
            })
            .await
            .map_err(|e| agent_status("cannot format the volume under lock", e))?;
        match outcome {
            Ok(()) => Ok(()),
            Err(FormatLockError::Reconcile(e @ ReconcileError::Busy { .. })) => {
                Err(Status::unavailable(e.to_string()))
            }
            Err(FormatLockError::Reconcile(e)) => {
                Err(Status::unavailable(format!("cannot acquire volume file lock: {e}")))
            }
            Err(FormatLockError::ForeignFs(fs)) => Err(Status::failed_precondition(format!(
                "volume {volume:?} acquired filesystem {fs:?} concurrently; refusing to overwrite it"
            ))),
            Err(FormatLockError::Mount(e)) => Err(mount_status(e)),
        }
    }
```

- [x] **Шаг 4: Прогнать проверки**

Выполнить:
```bash
cargo fmt --check
cargo clippy --all-targets --no-deps -- -D warnings
cargo test
```
Ожидаемо: PASS.

---

### Задача 9: Инициализатор PID 1 `tini` в образе и `DaemonSet` против зомби-сирот (`К2`)

**Файлы:**
- Модифицировать: `image/Dockerfile:48-55`
- Модифицировать: `deploy/daemonset.yaml:47-55`
- Тесты: `src/tests/main.rs`

**Интерфейсы:**
- Контейнер `ambar` запускается с `command: ["/usr/bin/tini", "--", "ambar", "agent"]`; в `image/Dockerfile` установлен пакет `tini`.

- [x] **Шаг 1: Написать падающий тест контракта манифеста и Dockerfile в `src/tests/main.rs`**

```rust
#[test]
fn container_runs_under_tini_as_pid1_subreaper() {
    let dockerfile = include_str!("../../image/Dockerfile");
    assert!(
        dockerfile.contains("tini"),
        "image/Dockerfile must install tini to reap orphaned drbd grandchildren"
    );
    let daemonset = include_str!("../../deploy/daemonset.yaml");
    assert!(
        daemonset.contains("[\"/usr/bin/tini\", \"--\", \"ambar\", \"agent\"]"),
        "deploy/daemonset.yaml must launch ambar agent under /usr/bin/tini"
    );
}
```

- [x] **Шаг 2: Запустить тест и убедиться в падении**

Выполнить: `cargo test container_runs_under_tini_as_pid1_subreaper`  
Ожидаемо: FAIL (`image/Dockerfile must install tini...`).

- [x] **Шаг 3: Обновить `image/Dockerfile` и `deploy/daemonset.yaml`**

В `image/Dockerfile`:
```dockerfile
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      kmod util-linux e2fsprogs libkeyutils1 lvm2 tini \
 && rm -rf /var/lib/apt/lists/* \
```

В `deploy/daemonset.yaml`:
```yaml
        - name: ambar
          image: AMBAR_IMAGE
          imagePullPolicy: AMBAR_IMAGE_PULL_POLICY
          # tini runs as PID 1 to reap orphaned drbdmeta/drbdsetup grandchildren
          # left behind if a command group times out, while forwarding SIGTERM
          # cleanly to `ambar agent` without touching its waitid(WNOWAIT) children.
          command: ["/usr/bin/tini", "--", "ambar", "agent"]
```

- [x] **Шаг 4: Финальная сквозная верификация и обновление индекса**

Выполнить:
```bash
python3 scripts/code-index.py
cargo fmt --check
cargo clippy --all-targets --no-deps -- -D warnings
cargo test
```
Ожидаемо: все тесты, форматтер и `clippy` проходят с нулём предупреждений.
